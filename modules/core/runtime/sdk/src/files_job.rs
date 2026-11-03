//! Pure fixed-memory listing lifecycle barriers. No I/O, callbacks, module-memory
//! access, references to caller buffers, or public C ABI are implemented here.
//! Request and row types must be owned values (Copy alone cannot establish that
//! embedded pointers refer to owned storage). All methods require external
//! serialization. Take an owned lease, release the model borrow, perform foreign
//! I/O/callback work, then return that exact lease. Never hold a critical section
//! or model borrow across foreign code. The loader must retire on teardown AND
//! failed init, then wait for quiescence before freeing module code/context.
use crate::{E_BUSY, E_FORMAT, E_NOT_FOUND, E_TOO_BIG};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(u32);
impl Handle {
    /// Wrap an untrusted identifier without granting validity. Broker checks it
    /// against live records; zero, unknown and retired IDs fail. Session routing
    /// and module attribution at the external boundary remain caller obligations.
    pub const fn from_id(id: u32) -> Self {
        Self(id)
    }
    pub const fn id(self) -> u32 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket(u32);
impl Ticket {
    /// Wrap an untrusted identifier; Broker checks it against the live client's
    /// current request, so zero, forged, canceled, and superseded IDs fail.
    pub const fn from_id(id: u32) -> Self {
        Self(id)
    }
    pub const fn id(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll<Row: Copy> {
    Pending,
    Row(Row),
    Done,
    Error(i32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<Row: Copy> {
    Pending,
    Row(Row),
    Done,
    Error(i32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventState {
    Row,
    Done,
    Error(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    owner: usize,
    handle: Handle,
    ticket: Ticket,
    nonce: u32,
}

/// Identity is the private nonce plus owner/handle/ticket, not request contents.
/// Copies of a returned lease cannot finish a later work step of the same request.
#[derive(Clone, Copy, Debug)]
pub struct WorkLease<Request: Copy> {
    pub owner: usize,
    pub handle: Handle,
    pub ticket: Ticket,
    pub request: Request,
    nonce: u32,
}
impl<Request: Copy> WorkLease<Request> {
    fn key(self) -> Key {
        Key {
            owner: self.owner,
            handle: self.handle,
            ticket: self.ticket,
            nonce: self.nonce,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct EventLease {
    pub owner: usize,
    pub handle: Handle,
    pub ticket: Ticket,
    pub state: EventState,
    nonce: u32,
}
impl EventLease {
    fn key(self) -> Key {
        Key {
            owner: self.owner,
            handle: self.handle,
            ticket: self.ticket,
            nonce: self.nonce,
        }
    }
}

#[derive(Clone, Copy)]
struct Client<Request: Copy, Row: Copy> {
    handle: Option<Handle>,
    live: bool,
    request: Option<(Ticket, Request)>,
    result: Poll<Row>,
    work: Option<Key>,
    event: Option<(Ticket, EventState)>,
    callback: Option<(Key, EventState)>,
}
impl<Request: Copy, Row: Copy> Client<Request, Row> {
    const fn new() -> Self {
        Self {
            handle: None,
            live: false,
            request: None,
            result: Poll::Pending,
            work: None,
            event: None,
            callback: None,
        }
    }
    fn reap(&mut self) {
        if !self.live && self.work.is_none() && self.callback.is_none() {
            *self = Self::new();
        }
    }
}

/// One client per runtime module-record index. Handles, tickets, and lease
/// nonces share a session-wide positive i32 ID space and are never reused.
/// Exhaustion fails before changing client state; constructing a new Broker
/// starts a new session, so old identities/leases must never cross brokers.
pub struct Broker<const CLIENTS: usize, Request: Copy, Row: Copy> {
    clients: [Client<Request, Row>; CLIENTS],
    next_id: u32,
    next_work: usize,
    next_event: usize,
}
impl<const CLIENTS: usize, Request: Copy, Row: Copy> Broker<CLIENTS, Request, Row> {
    pub const fn new() -> Self {
        Self {
            clients: [Client::new(); CLIENTS],
            next_id: 1,
            next_work: 0,
            next_event: 0,
        }
    }
    fn allocate(&mut self) -> Result<u32, i32> {
        if self.next_id == 0 {
            return Err(E_TOO_BIG);
        }
        let id = self.next_id;
        self.next_id = if id == i32::MAX as u32 { 0 } else { id + 1 };
        Ok(id)
    }
    fn client(&self, handle: Handle) -> Result<usize, i32> {
        self.clients
            .iter()
            .position(|c| c.live && c.handle == Some(handle))
            .ok_or(E_NOT_FOUND)
    }
    fn current(&self, handle: Handle, ticket: Ticket) -> Result<usize, i32> {
        let owner = self.client(handle)?;
        if self.clients[owner].request.map(|r| r.0) != Some(ticket) {
            return Err(E_NOT_FOUND);
        }
        Ok(owner)
    }
    pub fn attach(&mut self, owner: usize) -> Result<Handle, i32> {
        let client = self.clients.get(owner).ok_or(E_NOT_FOUND)?;
        if client.live {
            return Ok(client.handle.unwrap());
        }
        if client.handle.is_some() {
            return Err(E_BUSY);
        }
        let handle = Handle(self.allocate()?);
        self.clients[owner].handle = Some(handle);
        self.clients[owner].live = true;
        Ok(handle)
    }
    /// Supersede queued state immediately; active work/callback leases stay pinned.
    pub fn begin(&mut self, handle: Handle, request: Request) -> Result<Ticket, i32> {
        let owner = self.client(handle)?;
        let ticket = Ticket(self.allocate()?);
        let client = &mut self.clients[owner];
        client.request = Some((ticket, request));
        client.result = Poll::Pending;
        client.event = None;
        Ok(ticket)
    }
    pub fn cancel(&mut self, handle: Handle, ticket: Ticket) -> Result<(), i32> {
        let owner = self.current(handle, ticket)?;
        let client = &mut self.clients[owner];
        client.request = None;
        client.result = Poll::Pending;
        client.event = None;
        Ok(())
    }
    /// A row is copied out and consumed. End/error are sticky until begin/cancel.
    /// Observation clears a still-queued matching hint, never an active callback.
    pub fn poll(&mut self, handle: Handle, ticket: Ticket) -> Result<Poll<Row>, i32> {
        let owner = self.current(handle, ticket)?;
        let client = &mut self.clients[owner];
        let result = client.result;
        let observed = match result {
            Poll::Pending => None,
            Poll::Row(_) => Some(EventState::Row),
            Poll::Done => Some(EventState::Done),
            Poll::Error(error) => Some(EventState::Error(error)),
        };
        if observed.is_some() && client.event == observed.map(|state| (ticket, state)) {
            client.event = None;
        }
        if matches!(result, Poll::Row(_)) {
            client.result = Poll::Pending;
        }
        Ok(result)
    }
    pub fn take_work(&mut self) -> Result<Option<WorkLease<Request>>, i32> {
        if CLIENTS == 0 {
            return Ok(None);
        }
        for distance in 0..CLIENTS {
            let owner = (self.next_work + distance) % CLIENTS;
            let client = &self.clients[owner];
            if !client.live || client.work.is_some() || !matches!(client.result, Poll::Pending) {
                continue;
            }
            if let Some((ticket, request)) = client.request {
                let handle = client.handle.unwrap();
                let nonce = self.allocate()?;
                let key = Key {
                    owner,
                    handle,
                    ticket,
                    nonce,
                };
                self.clients[owner].work = Some(key);
                self.next_work = (owner + 1) % CLIENTS;
                return Ok(Some(WorkLease {
                    owner,
                    handle,
                    ticket,
                    request,
                    nonce,
                }));
            }
        }
        Ok(None)
    }
    /// Return the exact active lease. Stale-but-active completions release their
    /// pin and return false without changing newer requests. Invalid leases and
    /// nonnegative Error outcomes fail without releasing any pin.
    pub fn finish_work(
        &mut self,
        lease: WorkLease<Request>,
        outcome: Outcome<Row>,
    ) -> Result<bool, i32> {
        let key = lease.key();
        let client = self.clients.get_mut(key.owner).ok_or(E_NOT_FOUND)?;
        if client.handle != Some(key.handle) || client.work != Some(key) {
            return Err(E_NOT_FOUND);
        }
        if let Outcome::Error(error) = outcome {
            if error >= 0 {
                return Err(E_FORMAT);
            }
        }
        client.work = None;
        let publish = client.live && client.request.map(|r| r.0) == Some(key.ticket);
        if publish {
            client.result = match outcome {
                Outcome::Pending => Poll::Pending,
                Outcome::Row(row) => Poll::Row(row),
                Outcome::Done => Poll::Done,
                Outcome::Error(error) => Poll::Error(error),
            };
            let edge = match outcome {
                Outcome::Pending => None,
                Outcome::Row(_) => Some(EventState::Row),
                Outcome::Done => Some(EventState::Done),
                Outcome::Error(error) => Some(EventState::Error(error)),
            };
            if let Some(state) = edge {
                client.event = Some((key.ticket, state));
            }
        }
        client.reap();
        Ok(publish)
    }
    /// Deferred readiness only: this method never invokes module code. A callback
    /// may poll/cancel/begin after taking this owned lease and releasing the borrow.
    /// Hints coalesce; another poll may already have consumed this readiness.
    /// The callback must poll its ticket, which may have become stale meanwhile.
    pub fn take_event(&mut self) -> Result<Option<EventLease>, i32> {
        if CLIENTS == 0 {
            return Ok(None);
        }
        for distance in 0..CLIENTS {
            let owner = (self.next_event + distance) % CLIENTS;
            let client = &self.clients[owner];
            if !client.live || client.callback.is_some() {
                continue;
            }
            if let Some((ticket, state)) = client.event {
                if client.request.map(|r| r.0) != Some(ticket) {
                    self.clients[owner].event = None;
                    continue;
                }
                let handle = client.handle.unwrap();
                let nonce = self.allocate()?;
                let key = Key {
                    owner,
                    handle,
                    ticket,
                    nonce,
                };
                self.clients[owner].event = None;
                self.clients[owner].callback = Some((key, state));
                self.next_event = (owner + 1) % CLIENTS;
                return Ok(Some(EventLease {
                    owner,
                    handle,
                    ticket,
                    state,
                    nonce,
                }));
            }
        }
        Ok(None)
    }
    /// Release only this exact active callback pin; never clear a newer queued
    /// edge or mutate a reentrant replacement request.
    pub fn finish_event(&mut self, lease: EventLease) -> Result<(), i32> {
        let key = lease.key();
        let client = self.clients.get_mut(key.owner).ok_or(E_NOT_FOUND)?;
        if client.handle != Some(key.handle) || client.callback != Some((key, lease.state)) {
            return Err(E_NOT_FOUND);
        }
        client.callback = None;
        client.reap();
        Ok(())
    }
    /// Invalidates public identity immediately, including failed-init acquisition.
    /// Return true only when no provider or callback lease still pins the record.
    pub fn retire(&mut self, owner: usize) -> Result<bool, i32> {
        let client = self.clients.get_mut(owner).ok_or(E_NOT_FOUND)?;
        client.live = false;
        client.request = None;
        client.result = Poll::Pending;
        client.event = None;
        client.reap();
        self.quiescent(owner)
    }
    /// Loader free barrier: live clients are never quiescent, even without work.
    pub fn quiescent(&self, owner: usize) -> Result<bool, i32> {
        let client = self.clients.get(owner).ok_or(E_NOT_FOUND)?;
        Ok(!client.live && client.work.is_none() && client.callback.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type Jobs = Broker<3, [u8; 4], [u8; 8]>;
    fn work(jobs: &mut Jobs) -> WorkLease<[u8; 4]> {
        jobs.take_work().unwrap().unwrap()
    }

    #[test]
    fn supersede_and_cancel_during_work_drop_late_results_without_touching_new_request() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        assert_eq!(b.attach(0), Ok(h));
        let first = b.begin(h, [1; 4]).unwrap();
        let old = work(&mut b);
        let second = b.begin(h, [2; 4]).unwrap();
        assert_ne!(first, second);
        assert!(b.take_work().unwrap().is_none());
        assert_eq!(b.poll(h, first), Err(E_NOT_FOUND));
        assert!(!b.finish_work(old, Outcome::Row([1; 8])).unwrap());
        assert_eq!(b.poll(h, second), Ok(Poll::Pending));
        assert!(b.take_event().unwrap().is_none());
        let current = work(&mut b);
        assert_eq!(current.request, [2; 4]);
        b.cancel(h, second).unwrap();
        assert_eq!(b.poll(h, second), Err(E_NOT_FOUND));
        assert!(!b.finish_work(current, Outcome::Done).unwrap());
        assert!(b.take_work().unwrap().is_none());
        assert!(b.take_event().unwrap().is_none());
        assert_eq!(b.attach(0), Ok(h)); // cancel does not free the client
        assert!(b.begin(h, [3; 4]).unwrap().id() > second.id());
    }

    #[test]
    fn polled_rows_are_owned_and_poll_before_dispatch_suppresses_obsolete_hints() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [9; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([5; 8])).unwrap();
        let copied = match b.poll(h, t).unwrap() {
            Poll::Row(row) => row,
            _ => panic!(),
        };
        assert_eq!(b.poll(h, t), Ok(Poll::Pending));
        assert!(b.take_event().unwrap().is_none());
        let next = b.begin(h, [2; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([6; 8])).unwrap();
        assert_eq!(copied, [5; 8]);
        assert_eq!(b.poll(h, next), Ok(Poll::Row([6; 8])));
    }

    #[test]
    fn canceled_and_superseded_queued_events_never_dispatch() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([1; 8])).unwrap();
        b.cancel(h, t).unwrap();
        assert!(b.take_event().unwrap().is_none());
        let t = b.begin(h, [2; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Done).unwrap();
        let next = b.begin(h, [3; 4]).unwrap();
        assert!(b.take_event().unwrap().is_none());
        assert_eq!(b.poll(h, t), Err(E_NOT_FOUND));
        assert_eq!(b.poll(h, next), Ok(Poll::Pending));
    }

    #[test]
    fn reentrant_begin_cancel_and_active_event_return_preserve_new_edges() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([1; 8])).unwrap();
        let event = b.take_event().unwrap().unwrap();
        assert_eq!(event.state, EventState::Row);
        assert_eq!(b.poll(h, t), Ok(Poll::Row([1; 8])));
        b.cancel(h, t).unwrap();
        let next = b.begin(h, [2; 4]).unwrap();
        assert_eq!(b.poll(event.handle, event.ticket), Err(E_NOT_FOUND));
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([2; 8])).unwrap();
        assert!(b.take_event().unwrap().is_none()); // one active callback per client
        b.finish_event(event).unwrap();
        let new_event = b.take_event().unwrap().unwrap();
        assert_eq!(new_event.ticket, next);
        assert_eq!(b.poll(h, next), Ok(Poll::Row([2; 8])));
        b.finish_event(new_event).unwrap();
        assert!(b.take_event().unwrap().is_none());
    }

    #[test]
    fn polling_during_active_callback_can_advance_same_ticket_and_queue_new_hint() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([1; 8])).unwrap();
        let old = b.take_event().unwrap().unwrap();
        assert_eq!(b.poll(h, t), Ok(Poll::Row([1; 8])));
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Done).unwrap();
        b.finish_event(old).unwrap();
        let done = b.take_event().unwrap().unwrap();
        assert_eq!(done.state, EventState::Done);
        assert_eq!(b.poll(h, t), Ok(Poll::Done));
        b.finish_event(done).unwrap();
        assert!(b.take_work().unwrap().is_none());
    }

    #[test]
    fn retire_waits_for_both_pins_before_record_reuse_and_invalidates_identity_now() {
        let mut b = Jobs::new();
        let h = b.attach(1).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([1; 8])).unwrap();
        let event = b.take_event().unwrap().unwrap();
        b.poll(h, t).unwrap();
        let lease = work(&mut b);
        assert!(!b.retire(1).unwrap());
        assert!(!b.quiescent(1).unwrap());
        assert_eq!(b.attach(1), Err(E_BUSY));
        assert_eq!(b.begin(h, [2; 4]), Err(E_NOT_FOUND));
        assert_eq!(b.poll(h, t), Err(E_NOT_FOUND));
        assert_eq!(b.cancel(h, t), Err(E_NOT_FOUND));
        assert!(b.take_event().unwrap().is_none());
        assert!(!b.finish_work(lease, Outcome::Row([9; 8])).unwrap());
        assert!(!b.quiescent(1).unwrap());
        assert_eq!(b.attach(1), Err(E_BUSY));
        b.finish_event(event).unwrap();
        assert!(b.quiescent(1).unwrap());
        let fresh = b.attach(1).unwrap();
        assert_ne!(h, fresh);
        assert!(!b.quiescent(1).unwrap());
        assert_eq!(b.poll(h, t), Err(E_NOT_FOUND));
    }

    #[test]
    fn failed_init_retirement_needs_no_fini_and_suppresses_queued_results() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        assert!(b.retire(0).unwrap());
        assert!(b.quiescent(0).unwrap());
        let fresh = b.attach(0).unwrap();
        assert_ne!(h, fresh);
        b.begin(fresh, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Done).unwrap();
        assert!(b.retire(0).unwrap());
        assert!(b.take_event().unwrap().is_none());
        assert!(b.take_work().unwrap().is_none());
        assert!(b.quiescent(0).unwrap());
    }

    #[test]
    fn round_robin_work_and_events_respect_row_backpressure_without_blocking_others() {
        let mut b = Jobs::new();
        let handles = [
            b.attach(0).unwrap(),
            b.attach(1).unwrap(),
            b.attach(2).unwrap(),
        ];
        let tickets = [
            b.begin(handles[0], [0; 4]).unwrap(),
            b.begin(handles[1], [1; 4]).unwrap(),
            b.begin(handles[2], [2; 4]).unwrap(),
        ];
        for owner in 0..3 {
            let lease = work(&mut b);
            assert_eq!(lease.owner, owner);
            b.finish_work(lease, Outcome::Row([owner as u8; 8]))
                .unwrap();
        }
        assert!(b.take_work().unwrap().is_none());
        for owner in 0..3 {
            let event = b.take_event().unwrap().unwrap();
            assert_eq!(event.owner, owner);
            b.finish_event(event).unwrap();
        }
        assert_eq!(b.poll(handles[1], tickets[1]), Ok(Poll::Row([1; 8])));
        let lease = work(&mut b);
        assert_eq!(lease.owner, 1);
        b.finish_work(lease, Outcome::Pending).unwrap();
        b.poll(handles[0], tickets[0]).unwrap();
        let lease = work(&mut b);
        assert_eq!(lease.owner, 0);
        b.finish_work(lease, Outcome::Pending).unwrap();
        let lease = work(&mut b);
        assert_eq!(lease.owner, 1);
    }

    #[test]
    fn terminal_results_are_sticky_and_poll_clears_terminal_hints() {
        for outcome in [Outcome::Done, Outcome::Error(-3)] {
            let mut b = Jobs::new();
            let h = b.attach(0).unwrap();
            let t = b.begin(h, [1; 4]).unwrap();
            let lease = work(&mut b);
            b.finish_work(lease, outcome).unwrap();
            let expected = if matches!(outcome, Outcome::Done) {
                Poll::Done
            } else {
                Poll::Error(-3)
            };
            assert_eq!(b.poll(h, t), Ok(expected));
            assert_eq!(b.poll(h, t), Ok(expected));
            assert!(b.take_event().unwrap().is_none());
            assert!(b.take_work().unwrap().is_none());
        }
    }

    #[test]
    fn invalid_and_duplicate_leases_cannot_unpin_or_publish() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        let mut forged = lease;
        forged.nonce += 1;
        assert_eq!(b.finish_work(forged, Outcome::Done), Err(E_NOT_FOUND));
        assert!(b.take_work().unwrap().is_none());
        assert_eq!(b.finish_work(lease, Outcome::Error(0)), Err(E_FORMAT));
        assert_eq!(b.finish_work(lease, Outcome::Error(2)), Err(E_FORMAT));
        assert!(b.take_work().unwrap().is_none());
        assert_eq!(b.poll(h, t), Ok(Poll::Pending));
        b.finish_work(lease, Outcome::Pending).unwrap();
        let current = work(&mut b);
        assert_eq!(b.finish_work(lease, Outcome::Done), Err(E_NOT_FOUND));
        b.finish_work(current, Outcome::Row([1; 8])).unwrap();
        let event = b.take_event().unwrap().unwrap();
        let mut forged = event;
        forged.state = EventState::Done;
        assert_eq!(b.finish_event(forged), Err(E_NOT_FOUND));
        assert!(!b.retire(0).unwrap());
        assert_eq!(b.attach(0), Err(E_BUSY));
        b.finish_event(event).unwrap();
        assert_eq!(b.finish_event(event), Err(E_NOT_FOUND));
        assert!(b.quiescent(0).unwrap());
    }

    #[test]
    fn invalid_owners_handles_tickets_and_zero_capacity_fail_cleanly() {
        let mut empty = Broker::<0, u32, u32>::new();
        assert_eq!(empty.attach(0), Err(E_NOT_FOUND));
        assert!(empty.take_work().unwrap().is_none());
        assert!(empty.take_event().unwrap().is_none());
        assert_eq!(empty.retire(0), Err(E_NOT_FOUND));
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        assert_eq!(b.attach(3), Err(E_NOT_FOUND));
        assert_eq!(b.quiescent(3), Err(E_NOT_FOUND));
        assert_eq!(b.begin(Handle::from_id(0), [1; 4]), Err(E_NOT_FOUND));
        assert_eq!(b.poll(h, Ticket::from_id(0)), Err(E_NOT_FOUND));
        assert_eq!(b.cancel(h, Ticket::from_id(0)), Err(E_NOT_FOUND));
        assert_eq!(b.poll(Handle::from_id(h.id() + 100), t), Err(E_NOT_FOUND));
        assert_eq!(
            b.poll(Handle::from_id(u32::MAX), Ticket::from_id(t.id())),
            Err(E_NOT_FOUND)
        );
        assert_eq!(
            b.poll(Handle::from_id(h.id()), Ticket::from_id(u32::MAX)),
            Err(E_NOT_FOUND)
        );
        assert_eq!(
            b.poll(Handle::from_id(h.id()), Ticket::from_id(t.id())),
            Ok(Poll::Pending)
        );
    }

    #[test]
    fn session_ids_and_lease_nonces_exhaust_without_wrapping_or_destroying_existing_state() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        b.next_id = i32::MAX as u32 - 1;
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Row([1; 8])).unwrap();
        let event = b.take_event().unwrap().unwrap();
        assert_eq!(event.nonce, i32::MAX as u32);
        assert_eq!(b.begin(h, [2; 4]), Err(E_TOO_BIG));
        assert_eq!(b.attach(1), Err(E_TOO_BIG));
        assert_eq!(b.attach(0), Ok(h));
        assert_eq!(b.poll(h, t), Ok(Poll::Row([1; 8])));
        assert_eq!(b.take_work().err(), Some(E_TOO_BIG));
        b.finish_event(event).unwrap();
        assert!(b.retire(0).unwrap());
        assert_eq!(b.attach(0), Err(E_TOO_BIG));
        assert_eq!(b.next_id, 0);
        let mut b = Jobs::new();
        b.next_id = i32::MAX as u32;
        let last = b.attach(0).unwrap();
        assert_eq!(last.id(), i32::MAX as u32);
        assert_eq!(b.begin(last, [1; 4]), Err(E_TOO_BIG));
    }
    #[test]
    fn exhaustion_keeps_queued_events_and_last_ticket_identity_intact() {
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        let t = b.begin(h, [1; 4]).unwrap();
        let lease = work(&mut b);
        b.finish_work(lease, Outcome::Done).unwrap();
        b.next_id = 0;
        assert_eq!(b.take_event().err(), Some(E_TOO_BIG));
        assert_eq!(b.clients[0].event, Some((t, EventState::Done)));
        assert_eq!(b.poll(h, t), Ok(Poll::Done));
        assert!(b.retire(0).unwrap());
        let mut b = Jobs::new();
        let h = b.attach(0).unwrap();
        b.next_id = i32::MAX as u32;
        let last = b.begin(h, [2; 4]).unwrap();
        assert_eq!(last.id(), i32::MAX as u32);
        assert_eq!(b.begin(h, [3; 4]), Err(E_TOO_BIG));
        assert_eq!(b.poll(h, last), Ok(Poll::Pending));
        assert_eq!(b.take_work().err(), Some(E_TOO_BIG));
    }
}
