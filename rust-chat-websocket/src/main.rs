use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
    routing::get,
};
// use futures_util::StreamExt;
use tokio::{net::TcpListener, sync::broadcast};
#[derive(Clone)]
struct AppState {
    messages: broadcast::Sender<String>,
}
async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    // Each client gets its own receiver.
    let receiver = state.messages.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, state.messages, receiver))
}
async fn handle_socket(
    mut socket: WebSocket,
    sender: broadcast::Sender<String>,
    mut receiver: broadcast::Receiver<String>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        // Publish the message to all subscribers.
                        // The sending client also receives its own message.
                        let _ = sender.send(text.to_string());
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {
                        // Ignore binary and other non-text messages
                        // in this minimal chat example.
                    }
                    Some(Err(error)) => {
                        eprintln!("WebSocket read error: {error}");
                        break;
                    }
                }
            }
            outgoing = receiver.recv() => {
                match outgoing {
                    Ok(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    // A slow receiver may miss messages if the
                    // bounded broadcast buffer is overwritten.
                    // This demo skips that gap and continues.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}
#[tokio::main]
async fn main() {
    let (sender, _) = broadcast::channel::<String>(256);
    let state = AppState { messages: sender };
    let app = Router::new().route("/ws", get(websocket)).with_state(state);
    let listener = TcpListener::bind("127.0.0.1:3000")
        .await
        .expect("failed to bind address");
    println!("Rust chat server listening on 127.0.0.1:3000");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for Ctrl+C");
        })
        .await
        .expect("server error");
}
