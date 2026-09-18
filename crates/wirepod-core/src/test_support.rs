//! Fakes for the robot seam, behind the non-default `test-util` feature.
//!
//! Items behind `#[cfg(test)]` are not visible across a crate boundary, and
//! both this crate's own integration tests and `wirepod-server`'s handler tests
//! need these, so a feature is the mechanism. Consumers list `wirepod-core` a
//! second time as a dev-dependency with `features = ["test-util"]`.
//!
//! These stand in for the two seams Go opens for the same reason: the
//! `eventReceiver` interface (`server.go:637`) and the `enableImageStreaming`
//! function variable (`server.go:670`).
//!
//! Three items here are not fakes. `write_atomic_with_retry_budget` opens the
//! persistence path's Windows rename retry to a test that needs a budget it can
//! rely on, and exists only on Windows because the retry does.
//! `abort_write_after_filling_the_temporary` opens the window between the
//! temporary's creation and the rename, which is the window a crash leaves a
//! temporary in and which nothing that finishes a write can show. And
//! [`install_tracing_backstop`] makes the `tracing` callsite cache safe to
//! assert against from a parallel test harness.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::robot::conn::{
    BatteryReading, CameraControl, CameraFrame, ConnError, ConnTarget, EventItem, EventReceiver,
    FrameOutcome, FrameSink, FrameStream, JdocKind, NamedJdoc, ProtocolResult, ProtocolVerdict,
    RobotConn, RobotConnFactory, StatusCode, StimEvent,
};
use crate::robot::meter::CamMeter;

/// One call a [`FakeRobotConn`] recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RobotCall {
    /// `battery_state`.
    BatteryState,
    /// `protocol_version`, with the versions offered.
    ProtocolVersion {
        /// The version the caller claims to speak.
        client_version: i64,
        /// The lowest version the caller accepts.
        min_host_version: i64,
    },
    /// `open_event_stream`, with the filter it asked for.
    OpenEventStream {
        /// The event types requested.
        whitelist: Vec<String>,
        /// The connection id the stream was tagged with.
        connection_id: String,
    },
    /// `open_camera_feed`.
    OpenCameraFeed,
    /// `enable_image_streaming`, with the flag.
    EnableImageStreaming(bool),
    /// `pull_jdocs`, with the kinds it asked for.
    PullJdocs(Vec<JdocKind>),
}

type ReceiverResult = Result<Box<dyn EventReceiver>, ConnError>;
type FrameStreamResult = Result<Box<dyn FrameStream>, ConnError>;

struct FakeRobotState {
    battery: Result<BatteryReading, ConnError>,
    battery_delay: Duration,
    protocol: Result<ProtocolVerdict, ConnError>,
    protocol_gate: Option<Gate>,
    event_streams: Vec<ReceiverResult>,
    event_stream_gate: Option<Gate>,
    camera_feeds: Vec<FrameStreamResult>,
    camera_result: Result<(), ConnError>,
    jdocs: Result<Vec<NamedJdoc>, ConnError>,
    calls: Vec<RobotCall>,
}

/// A [`RobotConn`] whose answers are scripted and whose calls are recorded.
///
/// Streams are queued: each `open_*` call takes the next scripted result, and a
/// call past the end of the queue fails with `Unavailable` rather than
/// panicking, so a test that opens one stream too many sees it as a robot
/// failure.
pub struct FakeRobotConn {
    state: Mutex<FakeRobotState>,
}

impl Default for FakeRobotConn {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRobotConn {
    /// A robot that answers every call successfully and has no streams queued.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeRobotState {
                battery: Ok(BatteryReading::default()),
                battery_delay: Duration::ZERO,
                protocol: Ok(ProtocolVerdict {
                    result: ProtocolResult::Success,
                    host_version: 0,
                }),
                protocol_gate: None,
                event_streams: Vec::new(),
                event_stream_gate: None,
                camera_feeds: Vec::new(),
                camera_result: Ok(()),
                // One `vic.RobotSettings` document, because the seam never
                // answers with an empty list and the pinger indexes the first
                // entry (`sdkapp/jdocspinger.go:122-125`).
                jdocs: Ok(vec![NamedJdoc::default()]),
                calls: Vec::new(),
            }),
        }
    }

    /// Scripts what `battery_state` answers.
    #[must_use]
    pub fn with_battery(self, battery: Result<BatteryReading, ConnError>) -> Self {
        self.lock().battery = battery;
        self
    }

    /// Makes `battery_state` take `delay` before it answers.
    ///
    /// A delay no test can afford to wait out is how a robot that is powered
    /// off but whose IP still routes is driven: Go's connect-time liveness
    /// check has no deadline, so it hangs there forever (`robot.go:365`).
    #[must_use]
    pub fn with_battery_delay(self, delay: Duration) -> Self {
        self.lock().battery_delay = delay;
        self
    }

    /// Scripts what `protocol_version` answers.
    #[must_use]
    pub fn with_protocol(self, protocol: Result<ProtocolVerdict, ConnError>) -> Self {
        self.lock().protocol = protocol;
        self
    }

    /// Queues one answer for `open_event_stream`.
    #[must_use]
    pub fn with_event_stream(self, receiver: ReceiverResult) -> Self {
        self.lock().event_streams.push(receiver);
        self
    }

    /// Queues one answer for `open_camera_feed`.
    #[must_use]
    pub fn with_camera_feed(self, frames: FrameStreamResult) -> Self {
        self.lock().camera_feeds.push(frames);
        self
    }

    /// Scripts what `enable_image_streaming` answers.
    #[must_use]
    pub fn with_camera_result(self, result: Result<(), ConnError>) -> Self {
        self.lock().camera_result = result;
        self
    }

    /// Scripts what `pull_jdocs` answers.
    ///
    /// The list is handed back whole on every call, because the seam's contract
    /// is that an answer is either usable or an error; the two unusable shapes
    /// the wire allows are rejected in `wirepod-vector` and never reach a
    /// [`NamedJdoc`].
    #[must_use]
    pub fn with_jdocs(self, jdocs: Result<Vec<NamedJdoc>, ConnError>) -> Self {
        self.lock().jdocs = jdocs;
        self
    }

    /// Holds the next `protocol_version` call until the returned gate releases
    /// it.
    ///
    /// The call is recorded before it parks, so a test that waits on the gate
    /// resumes with the round trip genuinely in flight. That is what lets it
    /// move a [`ManualClock`](crate::clock::ManualClock) by a chosen amount
    /// across an RPC, and what lets it drive the probe deadline against a call
    /// that never answers.
    pub fn arm_protocol_gate(&self) -> Gate {
        let gate = Gate::new();
        self.lock().protocol_gate = Some(gate.clone());
        gate
    }

    /// Holds the next `open_event_stream` call until the returned gate releases
    /// it.
    ///
    /// The queued receiver is taken after the gate, so a caller abandoned while
    /// it is parked here leaves the receiver in the queue for the next one.
    pub fn arm_event_stream_gate(&self) -> Gate {
        let gate = Gate::new();
        self.lock().event_stream_gate = Some(gate.clone());
        gate
    }

    /// Every call made so far, in order.
    pub fn calls(&self) -> Vec<RobotCall> {
        self.lock().calls.clone()
    }

    /// How many calls of one kind the fake has answered.
    ///
    /// Counting rather than comparing the whole log is what a test wants when
    /// it is asserting that a second request opened no second stream, because
    /// the log also carries the connect-time `battery_state`.
    pub fn call_count(&self, matches: impl Fn(&RobotCall) -> bool) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|call| matches(call))
            .count()
    }

    fn lock(&self) -> MutexGuard<'_, FakeRobotState> {
        self.state.lock().expect("fake robot mutex poisoned")
    }

    fn exhausted(what: &str) -> ConnError {
        ConnError::new(StatusCode::Unavailable, format!("no {what} scripted"))
    }
}

#[async_trait]
impl CameraControl for FakeRobotConn {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        let mut state = self.lock();
        state.calls.push(RobotCall::EnableImageStreaming(on));
        state.camera_result.clone()
    }
}

#[async_trait]
impl RobotConn for FakeRobotConn {
    async fn battery_state(&self) -> Result<BatteryReading, ConnError> {
        let delay = {
            let mut state = self.lock();
            state.calls.push(RobotCall::BatteryState);
            state.battery_delay
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.lock().battery.clone()
    }

    async fn protocol_version(
        &self,
        client_version: i64,
        min_host_version: i64,
    ) -> Result<ProtocolVerdict, ConnError> {
        let gate = {
            let mut state = self.lock();
            state.calls.push(RobotCall::ProtocolVersion {
                client_version,
                min_host_version,
            });
            state.protocol_gate.take()
        };
        if let Some(gate) = gate {
            gate.pass().await;
        }
        self.lock().protocol.clone()
    }

    async fn open_event_stream(
        &self,
        whitelist: &[&str],
        connection_id: &str,
    ) -> Result<Box<dyn EventReceiver>, ConnError> {
        let gate = {
            let mut state = self.lock();
            state.calls.push(RobotCall::OpenEventStream {
                whitelist: whitelist.iter().map(|item| (*item).to_string()).collect(),
                connection_id: connection_id.to_string(),
            });
            state.event_stream_gate.take()
        };
        if let Some(gate) = gate {
            gate.pass().await;
        }
        let mut state = self.lock();
        if state.event_streams.is_empty() {
            return Err(Self::exhausted("event stream"));
        }
        Ok(state.event_streams.remove(0)?)
    }

    async fn open_camera_feed(&self) -> Result<Box<dyn FrameStream>, ConnError> {
        let mut state = self.lock();
        state.calls.push(RobotCall::OpenCameraFeed);
        if state.camera_feeds.is_empty() {
            return Err(Self::exhausted("camera feed"));
        }
        Ok(state.camera_feeds.remove(0)?)
    }

    async fn pull_jdocs(&self, kinds: &[JdocKind]) -> Result<Vec<NamedJdoc>, ConnError> {
        let mut state = self.lock();
        state.calls.push(RobotCall::PullJdocs(kinds.to_vec()));
        state.jdocs.clone()
    }
}

/// A one-shot hold on the next call of some kind.
///
/// The held call announces that it has arrived and then waits to be let go, so
/// a test can park a caller inside an RPC or a dial while it holds a lock. Both
/// halves store their permit, so neither side has to reach its await first.
#[derive(Clone, Debug)]
pub struct Gate {
    entered: Arc<Notify>,
    released: Arc<Notify>,
}

/// The name the camera fakes have always used for a [`Gate`].
pub type CameraGate = Gate;

impl Gate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            released: Arc::new(Notify::new()),
        }
    }

    /// Resolves once the gated call has arrived.
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    /// Lets the gated call carry on.
    pub fn release(&self) {
        self.released.notify_one();
    }

    async fn pass(&self) {
        self.entered.notify_one();
        self.released.notified().await;
    }
}

struct FakeFactoryState {
    results: VecDeque<Result<Arc<dyn RobotConn>, ConnError>>,
    targets: Vec<ConnTarget>,
    gate: Option<Gate>,
}

/// A [`RobotConnFactory`] that hands out configured connections, or fails, and
/// counts what it was asked to dial.
pub struct FakeConnFactory {
    state: Mutex<FakeFactoryState>,
}

impl FakeConnFactory {
    /// A factory that hands `conn` to every caller.
    pub fn connecting_to(conn: Arc<dyn RobotConn>) -> Self {
        Self::scripted(vec![Ok(conn)])
    }

    /// A factory whose dial always fails, which is how the preamble tests drive
    /// a robot that is not answering.
    pub fn failing(err: ConnError) -> Self {
        Self::scripted(vec![Err(err)])
    }

    /// A factory that answers `results` in order, the last entry repeating for
    /// every dial after it.
    ///
    /// [`FakeConnFactory::connecting_to`] and [`FakeConnFactory::failing`] are
    /// the one-entry cases. A longer script is what lets a test dial a serial
    /// that fails and then succeeds, which is how "a failed dial does not
    /// poison the serial's connect lock" is pinned.
    ///
    /// # Panics
    ///
    /// If `results` is empty, because a factory with nothing to answer would
    /// fail every dial for a reason no test wrote down.
    pub fn scripted(results: Vec<Result<Arc<dyn RobotConn>, ConnError>>) -> Self {
        assert!(
            !results.is_empty(),
            "a scripted factory needs at least one result"
        );
        Self {
            state: Mutex::new(FakeFactoryState {
                results: results.into(),
                targets: Vec::new(),
                gate: None,
            }),
        }
    }

    /// Holds the next dial until the returned gate releases it.
    ///
    /// Exactly one dial is held, so a dial for another serial runs normally
    /// while the held one is parked. That is what makes the per-serial connect
    /// lock observable: the second caller for the held serial waits, and a
    /// caller for any other serial does not.
    pub fn arm_connect_gate(&self) -> Gate {
        let gate = Gate::new();
        self.lock().gate = Some(gate.clone());
        gate
    }

    /// How many dials have been attempted.
    pub fn connect_count(&self) -> usize {
        self.lock().targets.len()
    }

    /// Every target dialled so far, in order.
    pub fn targets(&self) -> Vec<ConnTarget> {
        self.lock().targets.clone()
    }

    fn lock(&self) -> MutexGuard<'_, FakeFactoryState> {
        self.state.lock().expect("fake factory mutex poisoned")
    }
}

#[async_trait]
impl RobotConnFactory for FakeConnFactory {
    async fn connect(&self, target: &ConnTarget) -> Result<Arc<dyn RobotConn>, ConnError> {
        let gate = {
            let mut state = self.lock();
            state.targets.push(target.clone());
            state.gate.take()
        };
        if let Some(gate) = gate {
            gate.pass().await;
        }
        let result = {
            let mut state = self.lock();
            if state.results.len() > 1 {
                state.results.pop_front()
            } else {
                state.results.front().cloned()
            }
        };
        result.expect("a scripted factory ran out of results")
    }
}

#[derive(Debug, Default)]
struct LiveCount {
    live: usize,
    peak: usize,
}

/// Counts how many fakes are alive at once, and the highest that count ever
/// reached.
///
/// This is the Rust shape of Go's `liveReceivers` (`sdkapp_test.go:215-243`).
/// A [`FakeReceiver`] registered with one enters on construction and leaves on
/// drop, and because the stim loop owns its receiver and drops it as it
/// returns, the count is the number of live loops.
#[derive(Debug, Default)]
pub struct LiveCounter {
    state: Mutex<LiveCount>,
}

impl LiveCounter {
    /// A counter with nothing registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many are alive now.
    pub fn live(&self) -> usize {
        self.lock().live
    }

    /// The highest the live count ever reached.
    pub fn peak(&self) -> usize {
        self.lock().peak
    }

    fn enter(&self) {
        let mut state = self.lock();
        state.live += 1;
        state.peak = state.peak.max(state.live);
    }

    fn exit(&self) {
        self.lock().live -= 1;
    }

    fn lock(&self) -> MutexGuard<'_, LiveCount> {
        self.state.lock().expect("live counter mutex poisoned")
    }
}

/// One thing a [`FakeReceiver`] can be told to produce.
type FakeEvent = Result<Option<EventItem>, ConnError>;

/// An [`EventReceiver`] fed from a channel.
///
/// It has no idea cancellation exists, so nothing but the stim loop's own
/// `select!` can end it. That is stronger than what Go's `fakeReceiver` proves
/// (`sdkapp_test.go:187-214`), which selects on the context and therefore ends
/// itself.
///
/// It also signals when it first reaches its receive, which is what lets a test
/// stop a receiver that is genuinely parked rather than one that has not been
/// polled yet.
pub struct FakeReceiver {
    events: mpsc::UnboundedReceiver<FakeEvent>,
    counter: Option<Arc<LiveCounter>>,
    ready: Option<oneshot::Sender<()>>,
    _dropped: oneshot::Sender<()>,
}

/// The write end of a [`FakeReceiver`], plus its readiness and drop signals.
pub struct FakeReceiverHandle {
    events: mpsc::UnboundedSender<FakeEvent>,
    ready: Option<oneshot::Receiver<()>>,
    dropped: oneshot::Receiver<()>,
}

impl FakeReceiver {
    /// A receiver and its handle.
    pub fn new() -> (Self, FakeReceiverHandle) {
        Self::build(None)
    }

    /// The same, registered with `counter` for as long as the receiver lives.
    pub fn counted(counter: Arc<LiveCounter>) -> (Self, FakeReceiverHandle) {
        Self::build(Some(counter))
    }

    fn build(counter: Option<Arc<LiveCounter>>) -> (Self, FakeReceiverHandle) {
        if let Some(counter) = counter.as_ref() {
            counter.enter();
        }
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        (
            Self {
                events: events_rx,
                counter,
                ready: Some(ready_tx),
                _dropped: dropped_tx,
            },
            FakeReceiverHandle {
                events: events_tx,
                ready: Some(ready_rx),
                dropped: dropped_rx,
            },
        )
    }
}

impl Drop for FakeReceiver {
    fn drop(&mut self) {
        if let Some(counter) = self.counter.as_ref() {
            counter.exit();
        }
    }
}

#[async_trait]
impl EventReceiver for FakeReceiver {
    async fn next(&mut self) -> Result<Option<EventItem>, ConnError> {
        // Fired before the first receive is awaited, so a test that waits on it
        // resumes with this receiver already parked rather than merely spawned.
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(());
        }
        // No channel left to feed it means the robot went quiet forever, which
        // is exactly the state Go test 4 parks a receiver in.
        self.events.recv().await.unwrap_or(Ok(None))
    }
}

impl FakeReceiverHandle {
    /// Queues a stimulation event.
    pub fn send_stim(&self, value: f32, velocity: f32) {
        self.send(Ok(Some(EventItem::Stim(StimEvent { value, velocity }))));
    }

    /// Queues an event the stim loop should ignore.
    pub fn send_other(&self) {
        self.send(Ok(Some(EventItem::Other)));
    }

    /// Queues a clean end of stream.
    pub fn end(&self) {
        self.send(Ok(None));
    }

    /// Queues a receive failure.
    pub fn fail(&self, err: ConnError) {
        self.send(Err(err));
    }

    /// Resolves once the receiver has reached its first receive, which for the
    /// stim loop means the loop is parked waiting for an event.
    ///
    /// The signal is a oneshot fired by the receiver itself rather than a poll
    /// of some flag, which is what the parity spec asks for. Calling it a second
    /// time is a no-op, because the first call takes the channel.
    ///
    /// Awaiting this before a stop is what makes the two teardown tests
    /// meaningful: without it the stop can land before the loop has ever been
    /// polled, and a loop that only samples cancellation at the top of its
    /// iteration would pass.
    pub async fn ready(&mut self) {
        if let Some(mut ready) = self.ready.take() {
            let _ = (&mut ready).await;
        }
    }

    /// Resolves once the receiver has been dropped, which for the stim loop
    /// means the loop returned.
    ///
    /// This borrows rather than consumes on purpose: dropping the handle closes
    /// the channel, and a closed channel is an end of stream, which would end
    /// the very loop the caller is trying to observe.
    pub async fn wait_dropped(&mut self) {
        let _ = (&mut self.dropped).await;
    }

    fn send(&self, event: FakeEvent) {
        // A closed channel means the receiver is already gone, which several
        // tests arrange on purpose.
        let _ = self.events.send(event);
    }
}

/// A [`FrameStream`] fed from a channel, shaped like [`FakeReceiver`].
pub struct FakeFrameStream {
    frames: mpsc::UnboundedReceiver<Result<Option<CameraFrame>, ConnError>>,
}

/// The write end of a [`FakeFrameStream`].
pub struct FakeFrameStreamHandle {
    frames: mpsc::UnboundedSender<Result<Option<CameraFrame>, ConnError>>,
}

impl FakeFrameStream {
    /// A frame stream and its handle.
    pub fn new() -> (Self, FakeFrameStreamHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { frames: rx }, FakeFrameStreamHandle { frames: tx })
    }
}

#[async_trait]
impl FrameStream for FakeFrameStream {
    async fn next(&mut self) -> Result<Option<CameraFrame>, ConnError> {
        self.frames.recv().await.unwrap_or(Ok(None))
    }
}

impl FakeFrameStreamHandle {
    /// Queues one frame.
    pub fn send_frame(&self, data: Vec<u8>) {
        let _ = self.frames.send(Ok(Some(CameraFrame { data })));
    }

    /// Queues a clean end of stream.
    pub fn end(&self) {
        let _ = self.frames.send(Ok(None));
    }

    /// Queues a receive failure.
    pub fn fail(&self, err: ConnError) {
        let _ = self.frames.send(Err(err));
    }
}

/// One recorded camera call, stamped with its position in the fake's shared
/// order counter.
///
/// The stamp is what lets a test place a call against something that is not a
/// call, through [`RecordingCamera::stamp`]: the interesting question in a
/// handoff is whether the replacement's enable was still in flight when the
/// departing handler queued its release, and neither of those is visible in a
/// list of flags alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraCall {
    /// The flag the caller passed.
    pub on: bool,
    /// Where this call sits in the fake's order counter.
    pub order: u64,
}

struct CameraState {
    calls: Vec<CameraCall>,
    next_order: u64,
    enable_gate: Option<CameraGate>,
    disable_gate: Option<CameraGate>,
    enable_delay: Duration,
    disable_delay: Duration,
    result: Result<(), ConnError>,
}

impl CameraState {
    fn stamp(&mut self) -> u64 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }
}

/// A [`CameraControl`] that records the on and off calls in the order they
/// landed.
///
/// The order matters more than it looks. Go's fake burns a calibrated delay
/// before recording a disable and only then appends
/// (`sdkapp_test.go:73-78`), because the property under test is which RPC lands
/// last rather than which was issued first. The delays here are injected for
/// the same reason, and the recording happens after the delay.
#[derive(Clone)]
pub struct RecordingCamera {
    state: Arc<Mutex<CameraState>>,
}

impl Default for RecordingCamera {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingCamera {
    /// A camera that answers immediately and successfully.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(CameraState {
                calls: Vec::new(),
                next_order: 0,
                enable_gate: None,
                disable_gate: None,
                enable_delay: Duration::ZERO,
                disable_delay: Duration::ZERO,
                result: Ok(()),
            })),
        }
    }

    /// Holds the next enable call until the returned gate releases it.
    ///
    /// Exactly one call is held, so a later enable runs normally. A gate that is
    /// never released is how a test drives the enable deadline.
    pub fn arm_enable_gate(&self) -> CameraGate {
        let gate = CameraGate::new();
        self.lock().enable_gate = Some(gate.clone());
        gate
    }

    /// Holds the next disable call until the returned gate releases it.
    ///
    /// The mirror of [`RecordingCamera::arm_enable_gate`], and what parks a
    /// departing handler inside its disable while it still holds the camera
    /// operation lock. Exactly one call is held.
    pub fn arm_disable_gate(&self) -> CameraGate {
        let gate = CameraGate::new();
        self.lock().disable_gate = Some(gate.clone());
        gate
    }

    /// Takes the next number in the same order counter the calls are stamped
    /// from, so a test can place its own event among them.
    pub fn stamp(&self) -> u64 {
        self.lock().stamp()
    }

    /// Makes an enable take `delay` before it is recorded.
    #[must_use]
    pub fn with_enable_delay(self, delay: Duration) -> Self {
        self.lock().enable_delay = delay;
        self
    }

    /// Makes a disable take `delay` before it is recorded.
    #[must_use]
    pub fn with_disable_delay(self, delay: Duration) -> Self {
        self.lock().disable_delay = delay;
        self
    }

    /// Makes every call fail.
    #[must_use]
    pub fn with_error(self, err: ConnError) -> Self {
        self.lock().result = Err(err);
        self
    }

    /// The flags of the recorded calls, in the order they landed.
    pub fn calls(&self) -> Vec<bool> {
        self.lock().calls.iter().map(|call| call.on).collect()
    }

    /// The recorded calls with their order stamps.
    pub fn call_log(&self) -> Vec<CameraCall> {
        self.lock().calls.clone()
    }

    /// The flag of the call that landed last, if any.
    pub fn last_call(&self) -> Option<bool> {
        self.lock().calls.last().map(|call| call.on)
    }

    fn lock(&self) -> MutexGuard<'_, CameraState> {
        self.state.lock().expect("recording camera mutex poisoned")
    }
}

#[async_trait]
impl CameraControl for RecordingCamera {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        let (delay, gate) = {
            let mut state = self.lock();
            let delay = if on {
                state.enable_delay
            } else {
                state.disable_delay
            };
            let gate = if on {
                state.enable_gate.take()
            } else {
                state.disable_gate.take()
            };
            (delay, gate)
        };
        if let Some(gate) = gate {
            gate.pass().await;
        }
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let mut state = self.lock();
        let order = state.stamp();
        state.calls.push(CameraCall { on, order });
        state.result.clone()
    }
}

#[derive(Debug, Default)]
struct SinkState {
    frames: Vec<Vec<u8>>,
    outcomes: Vec<FrameOutcome>,
    scripted: VecDeque<FrameOutcome>,
    meter: Option<Arc<CamMeter>>,
    readings: Vec<(u64, u64)>,
    closed: bool,
}

/// A [`FrameSink`] that keeps what it was given and can be closed under the
/// pump, which is what a browser going away looks like.
pub struct RecordingSink {
    state: Arc<Mutex<SinkState>>,
}

/// A read-and-close handle on a [`RecordingSink`], usable while the pump owns
/// the sink itself.
#[derive(Clone, Debug, Default)]
pub struct SinkLog {
    state: Arc<Mutex<SinkState>>,
}

impl RecordingSink {
    /// A sink and its log.
    pub fn new() -> (Self, SinkLog) {
        let state = Arc::new(Mutex::new(SinkState::default()));
        (
            Self {
                state: Arc::clone(&state),
            },
            SinkLog { state },
        )
    }

    /// Records what `meter` reads at the moment each frame is handed over.
    ///
    /// This is how a test sees that the pump counts a frame before the sink ever
    /// touches it, which is the ordering Go relies on at `server.go:770-776`.
    #[must_use]
    pub fn watching(self, meter: Arc<CamMeter>) -> Self {
        self.state
            .lock()
            .expect("recording sink mutex poisoned")
            .meter = Some(meter);
        self
    }
}

#[async_trait]
impl FrameSink for RecordingSink {
    async fn send(&mut self, jpeg: &[u8]) -> FrameOutcome {
        let mut state = self.state.lock().expect("recording sink mutex poisoned");
        let outcome = if state.closed {
            FrameOutcome::Closed
        } else {
            state.scripted.pop_front().unwrap_or(FrameOutcome::Sent)
        };
        if outcome == FrameOutcome::Closed {
            state.closed = true;
            return FrameOutcome::Closed;
        }
        if let Some(reading) = state.meter.as_ref().map(|meter| meter.read()) {
            state.readings.push(reading);
        }
        state.frames.push(jpeg.to_vec());
        state.outcomes.push(outcome);
        outcome
    }
}

impl SinkLog {
    /// Every frame handed to the sink so far, including the ones it reported as
    /// skipped, because a skipped frame did reach the sink and only produced
    /// nothing on the wire.
    pub fn frames(&self) -> Vec<Vec<u8>> {
        self.lock().frames.clone()
    }

    /// How many frames have been handed over.
    pub fn frame_count(&self) -> usize {
        self.lock().frames.len()
    }

    /// What the sink answered for each of those frames.
    pub fn outcomes(&self) -> Vec<FrameOutcome> {
        self.lock().outcomes.clone()
    }

    /// What the watched meter read as each frame was handed over.
    pub fn readings(&self) -> Vec<(u64, u64)> {
        self.lock().readings.clone()
    }

    /// Queues the answer for one later frame, in order. Frames past the end of
    /// the queue are sent normally.
    pub fn script(&self, outcome: FrameOutcome) {
        self.lock().scripted.push_back(outcome);
    }

    /// Makes every later send report [`FrameOutcome::Closed`].
    pub fn close(&self) {
        self.lock().closed = true;
    }

    fn lock(&self) -> MutexGuard<'_, SinkState> {
        self.state.lock().expect("recording sink mutex poisoned")
    }
}

/// [`crate::persist::write_atomic`] with the Windows rename retry's budget
/// chosen by the caller, answering how many rename attempts it took.
///
/// The production budget is four attempts over fourteen milliseconds of
/// waiting, which is shorter than the few milliseconds it takes to create, fill
/// and `sync_all` the temporary, so a test that arranges a hold on the target
/// and then uses the production budget is racing its own setup: whether the
/// hold is still there when the first rename is attempted decides what the test
/// measures. A test that passes a wide budget drives the same loop with the
/// race gone.
///
/// The attempt count is what lets such a test say it exercised the retry. A
/// hold that has already cleared by the time the first rename runs produces a
/// perfectly good write in one attempt, and without the count that outcome is
/// indistinguishable from the one the test is there to check.
///
/// Windows only, because the retry is. `attempts` counts the first try, and
/// each wait after `first_backoff` is double the last.
#[cfg(windows)]
pub async fn write_atomic_with_retry_budget(
    path: impl Into<std::path::PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    attempts: u32,
    first_backoff: Duration,
) -> std::io::Result<u32> {
    crate::persist::write_atomic_with_budget(
        path,
        contents,
        mode,
        crate::persist::TemporaryIn::TargetDirectory,
        crate::persist::RetryBudget {
            attempts,
            first_backoff,
        },
    )
    .await
}

/// Creates and fills the temporary for `path` and stops, leaving it exactly
/// where a process that died before the rename would leave it, and answering
/// where that is.
///
/// This is the only way to observe what a crashed write leaves in a directory,
/// and the directory it leaves it in is a parity question rather than a tidiness
/// one: Go's `ReadSessionCerts` reads every name in `session-certs/` as a robot
/// ESN and dereferences `pem.Decode`'s nil result (`vars.go:377-388`), so a
/// temporary in there panics a rolled-back Go server at boot.
///
/// Unlike every failure path inside `write_atomic`, this deliberately does not
/// remove the temporary: the point is what stays behind.
pub async fn abort_write_after_filling_the_temporary(
    path: impl Into<std::path::PathBuf>,
    contents: impl Into<Vec<u8>>,
    mode: u32,
    temporary_in: crate::persist::TemporaryIn,
) -> std::io::Result<std::path::PathBuf> {
    crate::persist::fill_temporary_and_stop(path, contents, mode, temporary_in).await
}

/// Installs, once for the whole process, a global default `tracing` subscriber
/// that is live on every thread, so that no callsite can ever be cached as
/// uninteresting.
///
/// The problem this solves is a property of `tracing-core`, not of any one
/// test. Interest in a callsite is decided once, the first time that callsite
/// is reached, and cached globally for every thread
/// (`tracing-core-0.1.36/src/callsite.rs:236`, `:490-515`). While at most one
/// dispatcher has ever been registered, the rebuild takes a fast path that asks
/// only the calling thread's default dispatch (`callsite.rs:544-547`,
/// `:562-568`), and a thread with no default gets `NoSubscriber`, whose
/// `register_callsite` answers `Interest::never`. So a callsite first reached by
/// a test that installed no subscriber is cached as *never* for the rest of the
/// process, and every later test that asserts on that log line sees nothing.
/// The harness runs tests in parallel, so which test reaches a callsite first is
/// decided by the scheduler: `tests/config.rs` failed 102 runs out of 200 at two
/// threads over two of its boot tests before this existed.
///
/// A global default is the systematic fix because it is the one thing that is
/// live on *every* thread. `dispatcher::get_default` falls back to the global
/// when the calling thread has no scoped default, so with one installed the
/// fast path can no longer reach `NoSubscriber`, whichever thread gets there
/// first; and registering it rebuilds the interest of every callsite already
/// known, so a callsite poisoned before it existed is repaired.
///
/// Two earlier attempts were not that. A per-test ring installed with
/// `set_default` fixes only the thread it is installed on, so it cannot stop a
/// *different* test's thread from reaching the callsite first with nothing
/// installed. `rebuild_interest_cache` repairs callsites that already exist at
/// the moment it is called, which is a race against every callsite reached
/// afterwards rather than a rule about them. The global default is a rule.
///
/// The per-test `set_default` still belongs on top of this: a scoped default
/// wins over the global on its own thread, which is what lets each test read
/// its own ring back. The ring this installs is a throwaway that nothing reads,
/// bounded like every other [`crate::logger::LogRing`], and it exists only so
/// that the global default admits every callsite rather than rejecting it.
pub fn install_tracing_backstop() {
    static INSTALLED: Once = Once::new();

    INSTALLED.call_once(|| {
        use tracing_subscriber::layer::SubscriberExt;

        let ring = Arc::new(crate::logger::LogRing::new(Arc::new(
            crate::logger::ManualLogClock::new(0, "1970.01.01 00:00:00"),
        )));
        let backstop =
            tracing_subscriber::registry().with(crate::logger::LogLayer::new(Arc::clone(&ring)));
        // A failure means somebody else holds the global default, which is the
        // same guarantee by another route, so it is not worth reporting.
        let _ = tracing::subscriber::set_global_default(backstop);
        // Repairs every callsite that was reached, and possibly cached as
        // never, before the global existed. Callsites reached afterwards find
        // it through `get_default` and need no repair.
        tracing::callsite::rebuild_interest_cache();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::esn::Esn;
    use crate::robot::conn::BatteryLevel;
    use crate::robot::events::{EVENT_CONNECTION_ID, EVENT_WHITELIST};

    fn stim_stream_call() -> RobotCall {
        RobotCall::OpenEventStream {
            whitelist: vec!["stimulation_info".to_string()],
            connection_id: "wirepod".to_string(),
        }
    }

    #[tokio::test]
    async fn the_fake_robot_scripts_answers_and_records_calls() {
        let (receiver, _handle) = FakeReceiver::new();
        let robot = FakeRobotConn::new()
            .with_battery(Ok(BatteryReading {
                level: BatteryLevel::Nominal,
                volts: 4.1,
            }))
            .with_event_stream(Ok(Box::new(receiver)));

        let battery = robot.battery_state().await.expect("battery state failed");
        assert_eq!(battery.level, BatteryLevel::Nominal);
        assert!(
            robot
                .open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
                .await
                .is_ok()
        );
        // One stream was queued, so a second open reports the robot as
        // unavailable rather than panicking.
        assert!(
            robot
                .open_event_stream(EVENT_WHITELIST, EVENT_CONNECTION_ID)
                .await
                .is_err()
        );
        robot
            .enable_image_streaming(true)
            .await
            .expect("enable failed");

        assert_eq!(
            robot.calls(),
            vec![
                RobotCall::BatteryState,
                stim_stream_call(),
                stim_stream_call(),
                RobotCall::EnableImageStreaming(true),
            ]
        );
    }

    #[tokio::test]
    async fn the_fake_factory_counts_dials_and_can_fail() {
        let target = ConnTarget {
            esn: Esn::new("00E20100"),
            ip: "192.168.8.203".to_string(),
            guid: "<guid>".to_string(),
        };

        let factory = FakeConnFactory::connecting_to(Arc::new(FakeRobotConn::new()));
        assert!(factory.connect(&target).await.is_ok());
        assert!(factory.connect(&target).await.is_ok());
        assert_eq!(factory.connect_count(), 2);
        assert_eq!(factory.targets()[0].grpc_target(), "192.168.8.203:443");

        let failing = FakeConnFactory::failing(ConnError::deadline_exceeded());
        assert_eq!(
            failing.connect(&target).await.err(),
            Some(ConnError::deadline_exceeded())
        );
        assert_eq!(failing.connect_count(), 1);
    }

    #[tokio::test]
    async fn the_recording_camera_records_after_its_delay() {
        let camera = RecordingCamera::new().with_disable_delay(Duration::from_millis(20));
        let departing = camera.clone();
        let task = tokio::spawn(async move { departing.enable_image_streaming(false).await });

        // Let the disable reach its delay, so it is genuinely in flight when the
        // enable lands. Which call was issued first is not the property; which
        // one landed last is.
        tokio::task::yield_now().await;
        camera
            .enable_image_streaming(true)
            .await
            .expect("enable failed");
        task.await
            .expect("the disable task panicked")
            .expect("disable failed");

        assert_eq!(camera.calls(), vec![true, false]);
        assert_eq!(camera.last_call(), Some(false));
    }

    #[tokio::test]
    async fn the_recording_sink_reports_closed_once_closed() {
        let (mut sink, log) = RecordingSink::new();

        assert_eq!(sink.send(b"one").await, FrameOutcome::Sent);
        log.close();
        assert_eq!(sink.send(b"two").await, FrameOutcome::Closed);

        assert_eq!(log.frames(), vec![b"one".to_vec()]);
        assert_eq!(log.frame_count(), 1);
    }

    #[tokio::test]
    async fn the_fake_frame_stream_yields_then_ends() {
        let (mut frames, handle) = FakeFrameStream::new();
        handle.send_frame(vec![1, 2, 3]);
        handle.end();

        assert_eq!(
            frames.next().await.expect("frame failed"),
            Some(CameraFrame {
                data: vec![1, 2, 3]
            })
        );
        assert_eq!(frames.next().await.expect("end of stream failed"), None);
    }
}
