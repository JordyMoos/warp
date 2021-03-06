#![deny(warnings)]
extern crate futures;
extern crate pretty_env_logger;
extern crate warp;
extern crate serde;
extern crate serde_json;
#[macro_use] extern crate serde_derive;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

use futures::{Future, Stream};
use futures::sync::mpsc;
use warp::Filter;
use warp::ws::{Message, WebSocket};

/// Our global unique user id counter.
static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

/// Our state of currently connected users.
///
/// - Key is their id
/// - Value is a sender of `warp::ws::Message`
type Users = Arc<Mutex<HashMap<usize, mpsc::UnboundedSender<Message>>>>;


#[derive(Serialize, Deserialize, Clone, Debug)]
enum ChatMessage {
    Send { text: String },
}


#[derive(Serialize, Deserialize, Clone, Debug)]
enum Responses {
    NewMessage { by: String, text: String },
}


fn main() {
    pretty_env_logger::init();

    // Keep track of all connected users, key is usize, value
    // is a websocket sender.
    let users = Arc::new(Mutex::new(HashMap::new()));
    // Turn our "state" into a new Filter...
    let users = warp::any().map(move || users.clone());


    // GET /chat -> websocket upgrade
    let chat = warp::path("chat")
        // The `ws2()` filter will prepare Websocket handshake...
        .and(warp::ws2())
        .and(users)
        .map(|ws: warp::ws::Ws2, users| {
            // This will call our function if the handshake succeeds.
            ws.on_upgrade(move |socket| {
                user_connected(socket, users)
            })
        });

    // GET / -> index html
    let index = warp::path::end()
        .map(|| {
            warp::http::Response::builder()
                .header("content-type", "text/html; charset=utf-8")
                .body(INDEX_HTML)
        });

    let routes = index.or(chat);

    warp::serve(routes)
        .run(([127, 0, 0, 1], 3030));
}

fn user_connected(ws: WebSocket, users: Users) -> impl Future<Item = (), Error = ()> {
    // Use a counter to assign a new unique ID for this user.
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    eprintln!("new chat user: {}", my_id);

    // Split the socket into a sender and receive of messages.
    let (user_ws_tx, user_ws_rx) = ws.split();

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the websocket...
    let (tx, rx) = mpsc::unbounded();
    warp::spawn(
        rx
            .map_err(|()| -> warp::Error { unreachable!("unbounded rx never errors") })
            .forward(user_ws_tx)
            .map(|_tx_rx| ())
            .map_err(|ws_err| eprintln!("websocket send error: {}", ws_err))
    );


    // Save the sender in our list of connected users.
    users
        .lock()
        .unwrap()
        .insert(my_id, tx);

    // Return a `Future` that is basically a state machine managing
    // this specific user's connection.

    // Make an extra clone to give to our disconnection handler...
    let users2 = users.clone();

    user_ws_rx
        // Every time the user sends a message, broadcast it to
        // all other users...
        .for_each(move |msg| {
            user_message(my_id, msg, &users);
            Ok(())
        })
        // for_each will keep processing as long as the user stays
        // connected. Once they disconnect, then...
        .then(move |result| {
            user_disconnected(my_id, &users2);
            result
        })
        // If at any time, there was a websocket error, log here...
        .map_err(move |e| {
            eprintln!("websocket error(uid={}): {}", my_id, e);
        })
}

fn user_message(my_id: usize, msg: Message, users: &Users) {
    // Skip any non-Text messages...
    let msg_bytes = msg.to_str().unwrap();

//    let new_msg = format!("<User#{}>: {}", my_id, msg);
    let msg_result: serde_json::Result<ChatMessage> = serde_json::from_str(msg_bytes);
    let new_msg: ChatMessage = if let Ok(t) = msg_result {
        t
    } else {
        eprintln!("Failed to decode: {:?}", msg_bytes);
        eprintln!("Failed to decode: {:?}", msg_result);
        return;
    };

    let text = match new_msg {
        ChatMessage::Send { text } => text,
    };

    let response = Responses::NewMessage {
        by : "someone".to_string(),
        text : text,
    };

    // New message from this user, send it to everyone else (except same uid)...
    //
    // We use `retain` instead of a for loop so that we can reap any user that
    // appears to have disconnected.
    for (&uid, tx) in users.lock().unwrap().iter() {
        if my_id != uid {
            match tx.unbounded_send(Message::text(

                serde_json::to_string(&response).unwrap())) {

                Ok(()) => (),
                Err(_disconnected) => {
                    // The tx is disconnected, our `user_disconnected` code
                    // should be happening in another task, nothing more to
                    // do here.
                }
            }
        }
    }
}

fn user_disconnected(my_id: usize, users: &Users) {
    eprintln!("good bye user: {}", my_id);

    // Stream closed up, so remove from the user list
    users
        .lock()
        .unwrap()
        .remove(&my_id);
}

static INDEX_HTML: &str = r#"
<!DOCTYPE html>
<html>
    <head>
        <title>Warp Chat</title>
    </head>
    <body>
        <h1>warp chat</h1>
        <div id="chat">
            <p><em>Connecting...</em></p>
        </div>
        <input type="text" id="text" />
        <button type="button" id="send">Send</button>
        <script type="text/javascript">
        var uri = 'ws://' + location.host + '/chat';
        var ws = new WebSocket(uri);

        function message(data) {
            var line = document.createElement('p');
            line.innerText = data;
            chat.appendChild(line);
        }

        ws.onopen = function() {
            chat.innerHTML = "<p><em>Connected!</em></p>";
        }

        ws.onmessage = function(msg) {
            var data = JSON.parse(msg.data);
            message(data.NewMessage.by + ": " + data.NewMessage.text);
        };

        send.onclick = function() {
            var msg = text.value;
            ws.send(JSON.stringify({"Send": {"text": msg}}));
            text.value = '';

            message('<You>: ' + msg);
        };
        </script>
    </body>
</html>
"#;
