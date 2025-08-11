use axum::{
    extract::ws::{WebSocketUpgrade, WebSocket, Message},
    extract::State,
    routing::get,
    Router, response::IntoResponse,
};
use std::{sync::Arc};
use dashmap::DashMap;
use tokio::sync::broadcast;
use tonlib_rs::{TonClient, block::TransactionId};
use serde::{Deserialize, Serialize};
use futures::{StreamExt, SinkExt};

#[derive(Clone)]
struct AppState {
    ton: Arc<TonClient>,
    subscriptions: Arc<DashMap<String, broadcast::Sender<Trace>>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Trace {
    tx_hash: String,
    utime: Option<u32>,
    from: Option<String>,
    to: Option<String>,
}

#[tokio::main]
async fn main() {
    let ton = TonClient::builder().build().await.unwrap();
    let state = AppState {
        ton: Arc::new(ton),
        subscriptions: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state);

    println!("Listening on 0.0.0.0:3000");
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    if let Some(Ok(Message::Text(addr))) = socket.next().await {
        let (tx, _rx) = state.subscriptions
            .entry(addr.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(100);
                // Запускаем отслеживание адреса (если канал только что создан)
                let ton = state.ton.clone();
                let tx_clone = tx.clone();
                let addr_clone = addr.clone();
                tokio::spawn(async move {
                    watch_address(ton, addr_clone, tx_clone).await;
                });
                tx
            })
            .clone();
        let mut rx = tx.subscribe();
        loop {
            match rx.recv().await {
                Ok(trace) => {
                    let msg = serde_json::to_string(&trace).unwrap();
                    if socket.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    }
}

async fn watch_address(ton: Arc<TonClient>, addr: String, tx: broadcast::Sender<Trace>) {
    use std::collections::HashSet;
    let mut seen_traces = HashSet::new();
    loop {
        // Получить последние транзакции по адресу
        let txs = match ton.get_transactions(&addr).await {
            Ok(txs) => txs,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        for tx in txs {
            let trace = get_trace(&ton, &tx).await;
            let trace_id = trace.tx_hash.clone();
            if seen_traces.insert(trace_id.clone()) {
                let _ = tx.send(trace);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn get_trace(ton: &TonClient, tx: &TransactionId) -> Trace {
    // Получаем основную транзакцию
    let tx_info = match ton.get_transaction(tx).await {
        Ok(info) => info,
        Err(_) => {
            return Trace {
                tx_hash: tx.hash.clone(),
                utime: None,
                from: None,
                to: None,
            };
        }
    };
    // Пример: извлекаем базовые поля (utime, from, to)
    let utime = tx_info.utime;
    let from = tx_info.in_msg.and_then(|msg| msg.source);
    let to = tx_info.in_msg.and_then(|msg| msg.destination);
    Trace {
        tx_hash: tx.hash.clone(),
        utime: Some(utime),
        from,
        to,
    }
}
