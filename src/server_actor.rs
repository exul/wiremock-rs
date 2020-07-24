use crate::mock_actor::MockActor;
use async_std::net::TcpListener;
use async_tungstenite::tungstenite::Message;
use bastion::prelude::*;
use futures::io::{AsyncReadExt, Cursor};
use futures::prelude::*;
use futures::{StreamExt, TryStreamExt};
use log::{debug, info, warn};
use std::net::SocketAddr;

#[derive(Clone)]
pub(crate) struct ServerActor<R> {
    pub(crate) actor_ref: ChildRef,
    pub(crate) address: SocketAddr,
}

impl<R> ServerActor<R> {
    pub(crate) async fn start(mock_actor: MockActor<R>) -> ServerActor {
        // Allocate a random port
        let listener = get_available_port()
            .await
            .expect("No free port - cannot start an HTTP mock server!");
        Self::start_with(listener, mock_actor)
    }

    pub(crate) fn start_with(listener: TcpListener, mock_actor: MockActor<R>) -> ServerActor<R> {
        let address = listener.local_addr().unwrap();
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
                                    async_std::task::spawn(listen(mock_actor, listener)).await;
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

async fn listen(mock_actor: ChildRef, listener: async_std::net::TcpListener) {
    let addr = format!("http://{}", listener.local_addr().unwrap());
    while let Some(stream) = listener.incoming().next().await {
        // For each incoming stream, spawn up a task.
        let stream = stream.unwrap();
        let addr = addr.clone();
        let actor = mock_actor.clone();
        async_std::task::spawn(async {
            if let Err(err) = accept(actor, addr, stream).await {
                warn!("{}", err);
            }
        });
    }
}

// Take a TCP stream, and convert it into sequential HTTP request / response pairs.
async fn accept(
    mock_actor: ChildRef,
    addr: String,
    stream: async_std::net::TcpStream,
) -> http_types::Result<()> {
    debug!("===== Starting new connection from {}", stream.peer_addr()?);

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
                info!("Received a message: {}", msg.to_text().unwrap(),);

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

    // async_h1::accept(&addr, stream.clone(), move |req| {
    //     let a = mock_actor.clone();
    //     async move {
    //         info!("Request: {:?}", req);
    //         let answer = (&a).ask_anonymously(req).unwrap();

    //         let response = msg! { answer.await.expect("Couldn't receive the answer."),
    //             msg: Response => msg;
    //             _: _ => Response::new(StatusCode::NotFound);
    //         };
    //         info!("Response: {:?}", response);
    //         Ok(response)
    //     }
    // })
    // .await?;
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
