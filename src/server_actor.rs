use crate::mock_actor::MockActor;
use async_std::net::TcpListener;
use async_tungstenite::tungstenite::Message;
use bastion::prelude::*;
use futures::io::{AsyncReadExt, Cursor};
use futures::prelude::*;
use futures::{StreamExt, TryStreamExt};
use http_types::{Response, StatusCode};
use log::{debug, info, warn};
use std::fmt;
use std::net::SocketAddr;

#[derive(Copy, Clone)]
pub(crate) enum Protocol {
    Http,
    Ws,
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Protocol::Http => writeln!(f, "http"),
            Protocol::Ws => writeln!(f, "ws"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ServerActor {
    pub(crate) actor_ref: ChildRef,
    pub(crate) address: SocketAddr,
}

impl ServerActor {
    pub(crate) async fn start(mock_actor: MockActor, protocol: Protocol) -> ServerActor {
        // Allocate a random port
        let listener = get_available_port()
            .await
            .expect("No free port - cannot start an HTTP mock server!");
        Self::start_with(listener, mock_actor, protocol)
    }

    pub(crate) fn start_with(
        listener: TcpListener,
        mock_actor: MockActor,
        protocol: Protocol,
    ) -> ServerActor {
        let address = listener.local_addr().unwrap();
        let protocol = protocol.clone();
        debug!("ADDR: {}", address);

        let server_actors = Bastion::children(|children: Children| {
            children
                .with_exec(move |ctx: BastionContext| {
                    async move {
                        loop {
                            msg! { ctx.recv().await?,
                                msg: (ChildRef, TcpListener) => {
                                    let (mock_actor, listener) = msg;
                                    debug!("Mock server started listening on {}!", listener.local_addr().unwrap());
                                    async_std::task::spawn(listen(mock_actor, listener, protocol)).await;
                                    debug!("Shutting down!");
                                };
                                _: _ => {
                                    warn!("Received a message I was not listening for.");
                                };
                            }
                        }
                    }
                })
        })
            .expect("Couldn't create the server actor.");
        // We actually started only one actor
        let server_actor = server_actors.elems()[0].clone();

        // Pass in the TcpListener to start receiving connections and the mock actor
        // ChildRef to know what to respond to requests
        server_actor
            .tell_anonymously((mock_actor.actor_ref, listener))
            .expect("Failed to post TcpListener and mock actor address.");

        ServerActor {
            actor_ref: server_actor,
            address,
        }
    }
}

async fn listen(mock_actor: ChildRef, listener: async_std::net::TcpListener, protocol: Protocol) {
    let mut is_connection_check = true;
    let addr = format!("{}://{}", protocol, listener.local_addr().unwrap());
    while let Some(stream) = listener.incoming().next().await {
        // The first connection is from the mock server to verify
        // that the TCP connection is ready. It's not a proper WS
        // connection, so the handshake would fail
        if is_connection_check {
            debug!("Received first connection");
            is_connection_check = false;
            continue;
        }
        // For each incoming stream, spawn up a task.
        let stream = stream.unwrap();
        let addr = addr.clone();
        let actor = mock_actor.clone();
        match protocol {
            Protocol::Http => {
                async_std::task::spawn(async {
                    if let Err(err) = accept_http(actor, addr, stream).await {
                        warn!("{}", err);
                    }
                });
            }
            Protocol::Ws => {
                async_std::task::spawn(async {
                    if let Err(err) = accept_ws(actor, addr, stream).await {
                        warn!("{}", err);
                    }
                });
            }
        }
    }
}

// Take a TCP stream, and convert it into sequential HTTP request / response pairs.
async fn accept_http(
    mock_actor: ChildRef,
    addr: String,
    stream: async_std::net::TcpStream,
) -> http_types::Result<()> {
    async_h1::accept(&addr, stream.clone(), move |req| {
        let a = mock_actor.clone();
        async move {
            info!("Request: {:?}", req);
            let answer = (&a).ask_anonymously(req).unwrap();

            let response = msg! { answer.await.expect("Couldn't receive the answer."),
                msg: Response => msg;
                _: _ => Response::new(StatusCode::NotFound);
            };
            info!("Response: {:?}", response);
            Ok(response)
        }
    })
    .await?;
    Ok(())
}

// Take a TCP stream, and convert it into sequential WS request / response pairs.
async fn accept_ws(
    mock_actor: ChildRef,
    addr: String,
    stream: async_std::net::TcpStream,
) -> http_types::Result<()> {
    debug!(
        "===== Starting new connection from {} {}",
        stream.peer_addr()?,
        addr
    );

    let mut buf = vec![0u8; 1024];
    let mut s = stream.clone();

    // let n = async_std::io::ReadExt::read(&mut s, &mut buf).await?;
    // debug!("N is: {}", n);

    debug!("Done with tcp init");

    let mut ws_stream = match async_tungstenite::accept_async(stream.clone()).await {
        Ok(ws_stream) => ws_stream,
        Err(err) => {
            warn!("Error: {}", err);
            return Ok(());
        }
    };
    debug!("Accepted");

    let (write, mut read) = ws_stream.split();
    debug!("Split");

    // let msg = ws_stream
    //     .next()
    //     .await
    //     .ok_or_else(|| "didn't receive anything")
    //     .unwrap()
    //     .unwrap();
    // error!("MMMSG: {}", msg);

    let res = read
        .try_for_each(|msg| {
            let a = mock_actor.clone();
            async_std::task::spawn(async move {
                match msg {
                    Message::Close(_) => {
                        info!("Received close message");
                        return;
                    }
                    Message::Text(_) => info!("Received text message"),
                    _ => {
                        warn!("Received unsupported message: {:?}", msg);
                        return;
                    }
                };
                info!("Received a message: {:?}", msg,);

                let answer = (&a).ask_anonymously(msg).unwrap();

                let response = msg! { answer.await.expect("Couldn't receive the answer."),
                    msg: Message => msg;
                    _: _ => Message::Text("Hello".to_string());
                };
                info!("Response: {:?}", response);
            });

            future::ok(())
        })
        .await;

    if let Err(err) = res {
        warn!("Error while reading websocket messages: {}", err);
    }

    Ok(())
}

/// Get a local TCP listener for an available port.
/// If no port is available, returns None.
async fn get_available_port() -> Option<TcpListener> {
    for port in 8000..9000 {
        // Check if the specified port if available.
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return Some(l),
            Err(_) => continue,
        }
    }
    None
}
