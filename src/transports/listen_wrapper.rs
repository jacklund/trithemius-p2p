use libp2p::{
    core::transport::{ListenerId, TransportError, TransportEvent},
    Multiaddr, Transport,
};
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

pub struct ListenWrapper<I>
where
    I: Transport + Unpin,
{
    inner: I,
    address_translation: HashMap<Multiaddr, Multiaddr>,
    address_rx: mpsc::Receiver<(Multiaddr, Multiaddr)>,
}

impl<I> ListenWrapper<I>
where
    I: Transport + Unpin,
{
    pub fn new(inner: I, address_rx: mpsc::Receiver<(Multiaddr, Multiaddr)>) -> Self {
        Self {
            inner,
            address_translation: HashMap::new(),
            address_rx,
        }
    }
}

impl<I> Transport for ListenWrapper<I>
where
    I: Transport + Unpin,
{
    type Output = I::Output;
    type Error = I::Error;
    type ListenerUpgrade = I::ListenerUpgrade;
    type Dial = I::Dial;

    fn listen_on(&mut self, addr: Multiaddr) -> Result<ListenerId, TransportError<Self::Error>> {
        while let Ok((actual_addr, published_addr)) = self.address_rx.try_recv() {
            self.address_translation.insert(actual_addr, published_addr);
        }

        self.inner.listen_on(addr)
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }

    fn dial(&mut self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.inner.dial(addr)
    }

    fn dial_as_listener(
        &mut self,
        addr: Multiaddr,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        self.inner.dial_as_listener(addr)
    }

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        let me = self.get_mut();
        let poll = Pin::new(&mut me.inner).poll(cx);
        if let Poll::Ready(TransportEvent::NewAddress {
            listener_id,
            ref listen_addr,
        }) = poll
        {
            match me.address_translation.get(&listen_addr) {
                Some(published_addr) => Poll::Ready(TransportEvent::NewAddress {
                    listener_id,
                    listen_addr: published_addr.clone(),
                }),
                _ => poll,
            }
        } else {
            poll
        }
    }

    fn address_translation(&self, listen: &Multiaddr, observed: &Multiaddr) -> Option<Multiaddr> {
        self.inner.address_translation(listen, observed)
    }
}
