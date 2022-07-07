//! Mock objects for Tokio's `AsyncRead` and `AsyncWrite`.
//!
//! This crate is an alternative to the mocks in [tokio-test](https://crates.io/crates/tokio-test).

#![deny(
dead_code,
arithmetic_overflow,
invalid_type_param_default,
missing_fragment_specifier,
mutable_transmutes,
no_mangle_const_items,
overflowing_literals,
patterns_in_fns_without_body,
pub_use_of_private_extern_crate,
unknown_crate_types,
const_err,
order_dependent_trait_objects,
illegal_floating_point_literal_pattern,
improper_ctypes,
late_bound_lifetime_arguments,
non_camel_case_types,
non_shorthand_field_patterns,
non_snake_case,
non_upper_case_globals,
no_mangle_generic_items,
private_in_public,
stable_features,
type_alias_bounds,
tyvar_behind_raw_pointer,
unconditional_recursion,
unused_comparisons,
unreachable_pub,
anonymous_parameters,
missing_copy_implementations,
//missing_debug_implementations,
missing_docs,
trivial_casts,
trivial_numeric_casts,
unused_import_braces,
unused_qualifications,
clippy::all
)]
#![forbid(
    unsafe_code,
    rustdoc::broken_intra_doc_links,
    unaligned_references,
    while_true,
    bare_trait_objects
)]

use std::collections::VecDeque;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

/// Create a Mock I/O object and a controlling Handle
pub fn mock() -> (Mock, Handle) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mock = Mock {
        actions: Default::default(),
        rx,
        tx: event_tx,
    };
    let handle = Handle { tx, rx: event_rx };
    (mock, handle)
}

/// Mock object that can be used in lieu of a socket, etc
pub struct Mock {
    // current queue of expected actions
    actions: VecDeque<Action>,
    // how additional actions can be received
    rx: tokio::sync::mpsc::UnboundedReceiver<Action>,
    // how events get pushed back to the test
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

/// Handle which can send actions to the Mock and monitor Event's as the mock consumes the actions
pub struct Handle {
    tx: tokio::sync::mpsc::UnboundedSender<Action>,
    rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
}

impl Handle {
    /// Queue a read operation on the Mock
    pub fn read(&mut self, data: &[u8]) {
        self.tx.send(Action::read(data)).unwrap()
    }

    /// Queue a read error on the Mock
    pub fn read_error(&mut self, kind: ErrorKind) {
        self.tx.send(Action::read_error(kind)).unwrap()
    }

    /// Queue a write error on the Mock
    pub fn write_error(&mut self, kind: ErrorKind) {
        self.tx.send(Action::write_error(kind)).unwrap()
    }

    /// Asynchronously wait for the next event
    pub async fn next_event(&mut self) -> Event {
        self.rx.recv().await.unwrap()
    }

    /// Pop the next event if present
    pub fn pop_event(&mut self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

/// events are things we queue up for the component under test
#[derive(Debug)]
enum Action {
    Read(Vec<u8>),
    ReadError(ErrorKind),
    WriteError(ErrorKind),
}

/// Events that is produced as the Mock consumes an action
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// write operation was performed
    Write(Vec<u8>),
    /// all of the data in a queued read was consumed
    Read,
    /// queued write error was returned by the mock
    WriteErr,
    /// queued read error was returned by the mock
    ReadErr,
}

impl Action {
    fn read(data: &[u8]) -> Self {
        Self::Read(data.to_vec())
    }

    fn read_error(kind: ErrorKind) -> Self {
        Self::ReadError(kind)
    }

    fn write_error(kind: ErrorKind) -> Self {
        Self::WriteError(kind)
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.rx.close();
        if let Ok(action) = self.rx.try_recv() {
            if !std::thread::panicking() {
                panic!("Unused mock action: {:?}", action)
            }
        }
    }
}

impl Mock {
    fn front(&mut self, cx: &mut Context) -> Option<&Action> {
        // we always poll the receiver
        if let Poll::Ready(action) = self.rx.poll_recv(cx) {
            match action {
                None => {
                    panic!("The sending side of the channel was closed");
                }
                Some(x) => {
                    self.actions.push_back(x);
                }
            }
        }

        self.actions.front()
    }
}

impl tokio::io::AsyncRead for Mock {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        match self.front(cx) {
            None => Poll::Pending,
            Some(action) => match action {
                Action::Read(bytes) => {
                    if buf.remaining() < bytes.len() {
                        panic!(
                            "Expecting a read for at least {} bytes but only space for {} bytes",
                            bytes.len(),
                            buf.remaining()
                        );
                    }
                    buf.put_slice(bytes.as_slice());
                    self.tx.send(Event::Read).unwrap();
                    self.actions.pop_front();
                    Poll::Ready(Ok(()))
                }
                Action::ReadError(kind) => {
                    let kind = *kind;
                    let ret = Poll::Ready(Err(kind.into()));
                    self.tx.send(Event::WriteErr).unwrap();
                    self.actions.pop_front();
                    ret
                }
                Action::WriteError(_) => Poll::Pending,
            },
        }
    }
}

impl tokio::io::AsyncWrite for Mock {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        match self.front(cx) {
            Some(Action::WriteError(kind)) => {
                let kind = *kind;
                self.tx.send(Event::WriteErr).unwrap();
                self.actions.pop_front();
                Poll::Ready(Err(kind.into()))
            }
            _ => {
                self.tx.send(Event::Write(buf.to_vec())).unwrap();
                Poll::Ready(Ok(buf.len()))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }
}
