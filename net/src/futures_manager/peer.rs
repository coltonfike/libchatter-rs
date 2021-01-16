use std::collections::VecDeque;
use futures::{SinkExt, stream};
use tokio::{
    io::{
        AsyncRead,
        AsyncWrite
    }
};
use tokio_util::codec::{
    Decoder, 
    Encoder, 
    FramedRead, 
    FramedWrite
};
use types::WireReady;
use tokio_stream::StreamExt;
use std::sync::Arc;
// use tokio::sync::mpsc::{
use futures::channel::mpsc::{
    UnboundedSender, 
    UnboundedReceiver, 
    // unbounded_channel,
    unbounded as unbounded_channel,
};

/// A Peer is a network object that abstracts as a type that is a stream of type
/// O, and is a sink of type I
///
/// The user of a peer can send messages of type I, and gets messages of type O
///
/// The types I and O must be thread safe, unpin, and can be encoded, decoded
/// into.
pub struct Peer<I,O> 
where I: WireReady,
O: WireReady,
{
    /// Send O msg to this peer
    pub send: UnboundedSender<Arc<O>>,
    /// Get I msg from this peer
    pub recv: UnboundedReceiver<I>,
}

enum InternalInMsg {
    Ready,
}

enum InternalOutMsg<O> {
    Batch(VecDeque<Arc<O>>),
}

impl<'de,I,O> Peer<I,O> 
where I: WireReady+'static+Sync+Unpin,
O: WireReady+'static + Clone+Sync,
{
    pub fn new(
        rd: impl AsyncRead + Unpin + Send + 'static,
        wr: impl AsyncWrite + Unpin + Send + 'static,
        d: impl Decoder<Item=I, Error=std::io::Error> + Send + 'static,
        e: impl Encoder<Arc<O>> + Send + 'static
    ) -> Self 
    {
        log::trace!(target:"net/peer", "Creating a new peer");
        // channels used by the peer to talk to the sockets: 
        // the send is used to get message from the outside and send it to the
        // network
        //
        // 
        let (mut send_in, recv_in) = unbounded_channel::<I>();
        let (send_out, mut recv_out) = unbounded_channel::<Arc<O>>();
        
        let mut reader = FramedRead::new(rd, d);
        let mut writer = FramedWrite::new(wr, e);
        let handle = tokio::runtime::Handle::current();
        let (mut internal_ch_in_send, mut internal_ch_in_recv) = 
            unbounded_channel();
        let (mut internal_ch_out_send, mut internal_ch_out_recv) = 
            unbounded_channel::<InternalOutMsg<O>>();
        handle.spawn(async move {
            loop {
                let opt = internal_ch_out_recv.next().await;
                if let Some(InternalOutMsg::Batch(to_send)) = opt {
                    let mut s = stream::iter(to_send.into_iter().map(Ok));
                    if let Err(_e) = writer.send_all(&mut s).await {
                        log::error!(target:"peer","Failed to write a message to a peer");
                        std::process::exit(0);
                    }
                    if let Err(_e) = internal_ch_in_send.send(InternalInMsg::Ready).await {
                        log::error!(target:"peer", "Failed to send a message to the internal channel");
                    }
                } else {
                    log::error!(target:"peer", "Internal message channel closed");
                    std::process::exit(0);
                }
            }
        });
        handle.spawn(async move {
            let mut buffers = VecDeque::new();
            // let mut write_task= FuturesUnordered::new();
            let mut ready = true;
            loop {
                tokio::select! {
                    in_opt = reader.next() => {
                        if let None = in_opt {
                            log::warn!(target:"peer", "Disconnected from peer");
                            std::process::exit(0);
                        }
                        if let Some(Ok(x)) = in_opt {
                            if let Err(_e) = send_in.send(x).await {
                                log::warn!(target:"peer", "Error in sending out");
                                std::process::exit(0);
                            }
                        }
                    },
                    out_opt = recv_out.next() => {
                        if let None = out_opt {
                            log::warn!(target:"peer", "Error in receiving message");
                            std::process::exit(0);
                        }
                        if let Some(x) = out_opt {
                            // Write if not already writing, otherwise
                            // buffer and try again later
                            if ready {
                                buffers.push_back(x);
                                if let Err(_e) = internal_ch_out_send.send(InternalOutMsg::Batch(buffers)).await {
                                    log::warn!(target:"net", "Error in sending message out");
                                    std::process::exit(0);
                                }
                                buffers = VecDeque::new();
                            } else {
                                buffers.push_back(x);
                            }
                        }
                    },
                    internal_ch_recv_opt = internal_ch_in_recv.next() => {
                        if let Some(InternalInMsg::Ready) = internal_ch_recv_opt {
                            ready = true;                                
                        } else {
                            log::warn!(target:"net", "Error in getting message from int channel");
                            std::process::exit(0);
                        }
                    }
                }
            }
        });
        
        Self {
            send: send_out,
            recv: recv_in,
        }
    }
}