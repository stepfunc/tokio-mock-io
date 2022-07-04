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

use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

/// Create a Mock I/O object and a controlling Handle
pub fn mock() -> (Mock, Handle) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mock = Mock {
        next: None,
        rx,
        tx: event_tx,
    };
    let handle = Handle { tx, rx: event_rx };
    (mock, handle)
}

/// Mock object that can be used in lieu of a socket, etc
pub struct Mock {
    // the current action
    next: Option<Action>,
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

    /// Queue a write operation on the Mock
    pub fn write(&mut self, data: &[u8]) {
        self.tx.send(Action::write(data)).unwrap()
    }

    /// Queue a read error on the Mock
    pub fn read_error(&mut self, kind: ErrorKind) {
        self.tx.send(Action::read_error(kind)).unwrap()
    }

    /// Queue a write error on the Mock
    pub fn write_error(&mut self, kind: ErrorKind) {
        self.tx.send(Action::write_error(kind)).unwrap()
    }

    /// Asynchronously wait for the next action to be consumed
    pub async fn next_event(&mut self) -> Event {
        self.rx.recv().await.unwrap()
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
enum Direction {
    Read,
    Write,
}

#[derive(Debug)]
enum ActionType {
    Data(Vec<u8>),
    Error(ErrorKind),
}

#[derive(Debug)]
struct Action {
    direction: Direction,
    action_type: ActionType,
}

impl Action {
    fn get_event(&self) -> Event {
        match self.direction {
            Direction::Read => match &self.action_type {
                ActionType::Data(x) => Event::Read(x.len()),
                ActionType::Error(x) => Event::ReadErr(*x),
            },
            Direction::Write => match &self.action_type {
                ActionType::Data(x) => Event::Write(x.len()),
                ActionType::Error(x) => Event::WriteErr(*x),
            },
        }
    }
}

/// Events that is produced as the Mock consumes an action
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Event {
    /// write operation was performed of the associated size
    Write(usize),
    /// read operation was performed of the associated size
    Read(usize),
    /// write error was returned by the mock
    WriteErr(ErrorKind),
    /// read error was returned by the mock
    ReadErr(ErrorKind),
}

impl Action {
    fn read(data: &[u8]) -> Self {
        Self {
            direction: Direction::Read,
            action_type: ActionType::Data(data.to_vec()),
        }
    }

    fn write(data: &[u8]) -> Self {
        Self {
            direction: Direction::Write,
            action_type: ActionType::Data(data.to_vec()),
        }
    }

    fn read_error(kind: ErrorKind) -> Self {
        Self {
            direction: Direction::Read,
            action_type: ActionType::Error(kind),
        }
    }

    fn write_error(kind: ErrorKind) -> Self {
        Self {
            direction: Direction::Write,
            action_type: ActionType::Error(kind),
        }
    }
}

impl Mock {
    fn pop_event(&mut self, dir: Direction, x: Action) -> Option<ActionType> {
        if x.direction == dir {
            self.tx.send(x.get_event()).unwrap();
            Some(x.action_type)
        } else {
            // it's not the right direction so store it
            self.next = Some(x);
            None
        }
    }

    fn pop(&mut self, dir: Direction, cx: &mut Context) -> Option<ActionType> {
        // if there is a pending action
        if let Some(x) = self.next.take() {
            return self.pop_event(dir, x);
        }

        if let Poll::Ready(action) = self.rx.poll_recv(cx) {
            match action {
                None => {
                    panic!("The sending side of the channel was closed");
                }
                Some(x) => {
                    return self.pop_event(dir, x);
                }
            }
        }

        None
    }
}

impl tokio::io::AsyncRead for Mock {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<std::io::Result<()>> {
        match self.pop(Direction::Read, cx) {
            None => Poll::Pending,
            Some(ActionType::Data(bytes)) => {
                if buf.remaining() < bytes.len() {
                    panic!(
                        "Expecting a read for {:?} but only space for {} bytes",
                        bytes.as_slice(),
                        buf.remaining()
                    );
                }
                buf.put_slice(bytes.as_slice());
                Poll::Ready(Ok(()))
            }
            Some(ActionType::Error(kind)) => Poll::Ready(Err(kind.into())),
        }
    }
}

impl tokio::io::AsyncWrite for Mock {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        match self.pop(Direction::Write, cx) {
            None => panic!("unexpected write: {:?}", buf),
            Some(ActionType::Data(bytes)) => {
                assert_eq!(bytes.as_slice(), buf);
                Poll::Ready(Ok(buf.len()))
            }
            Some(ActionType::Error(kind)) => Poll::Ready(Err(kind.into())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }
}
