//! The engine's side of what a test client's calls hand over.
//!
//! A client assembled from parts has no session behind it. What the engine
//! carries in its own loop — an order from the call to the wire, a question
//! answered from what the session holds — is handed to a loop of its own
//! here, and what that loop builds is read back as the commands it would
//! send. Everything else a call sends is read back as it was sent.

// Each test crate that includes this reads what it needs of it.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvError, Sender, TryRecvError};

use ibkr_dx::bridge::SharedState;
use ibkr_dx::engine::hot_loop::HotLoop;
use ibkr_dx::types::ControlCommand;

/// A test client's engine: the channel its calls write to, and the loop that
/// takes what the engine carries.
pub struct Engine {
    rx: Receiver<ControlCommand>,
    into: Sender<ControlCommand>,
    engine: RefCell<HotLoop>,
    out: RefCell<VecDeque<ControlCommand>>,
}

impl Engine {
    pub fn new(rx: Receiver<ControlCommand>, shared: &Arc<SharedState>) -> Self {
        let mut engine = HotLoop::new(shared.clone(), None, None);
        let (into, taken) = std::sync::mpsc::channel();
        engine.set_control_rx(taken);
        Self { rx, into, engine: RefCell::new(engine), out: RefCell::default() }
    }

    /// Give the engine a trading connection, and hand back the venue's end
    /// of it to read what the engine sends.
    pub fn with_trading(&self) -> std::net::TcpStream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let ours = std::net::TcpStream::connect(listener.local_addr().unwrap()).expect("the dial");
        let (venue, _) = listener.accept().expect("the venue's end");
        venue.set_read_timeout(Some(std::time::Duration::from_millis(200))).unwrap();
        self.engine.borrow_mut().ccp_conn =
            Some(ibkr_dx::protocol::connection::Connection::new_raw(ours).expect("a connection"));
        venue
    }

    /// Take one command as the engine takes it.
    fn take(&self, cmd: ControlCommand) {
        let carried = matches!(
            cmd,
            ControlCommand::Place(_)
                | ControlCommand::CancelOrder { .. }
                | ControlCommand::CancelOrderByPermId { .. }
                | ControlCommand::GlobalCancel { .. }
                | ControlCommand::Exercise(_)
                | ControlCommand::Bracket(_)
                | ControlCommand::Ask(_)
                | ControlCommand::Retire(_)
                | ControlCommand::FetchCompletedOrders { .. }
                | ControlCommand::Subscribe { .. }
                | ControlCommand::CancelMktData { .. }
                | ControlCommand::CancelCalculation { .. }
                | ControlCommand::SubscribeTbt { .. }
                | ControlCommand::UnsubscribeTbt { .. }
        );
        if !carried {
            self.out.borrow_mut().push_back(cmd);
            return;
        }
        // A market-data request is read back as it was asked, too: the loop
        // takes it rather than sending it on as it stands.
        if matches!(
            cmd,
            ControlCommand::Subscribe { .. }
                | ControlCommand::CancelMktData { .. }
                | ControlCommand::CancelCalculation { .. }
                | ControlCommand::SubscribeTbt { .. }
                | ControlCommand::UnsubscribeTbt { .. }
        ) {
            self.out.borrow_mut().push_back(cmd.clone());
        }
        let _ = self.into.send(cmd);
        let mut engine = self.engine.borrow_mut();
        engine.poll_once();
        let built: Vec<_> = engine.context_mut().drain_pending_orders().collect();
        self.out.borrow_mut().extend(built.into_iter().map(ControlCommand::Order));
    }

    /// Take everything the calls have sent so far.
    pub fn pump(&self) {
        while let Ok(cmd) = self.rx.try_recv() {
            self.take(cmd);
        }
    }

    pub fn try_recv(&self) -> Result<ControlCommand, TryRecvError> {
        self.pump();
        self.out.borrow_mut().pop_front().ok_or(TryRecvError::Empty)
    }

    pub fn try_iter(&self) -> impl Iterator<Item = ControlCommand> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }

    /// Wait for the next command, until every sender has gone.
    pub fn recv(&self) -> Result<ControlCommand, RecvError> {
        loop {
            if let Some(cmd) = self.out.borrow_mut().pop_front() {
                return Ok(cmd);
            }
            let cmd = self.rx.recv()?;
            self.take(cmd);
        }
    }
}
