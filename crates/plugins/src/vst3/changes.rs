//! What a plugin processes besides audio: a block's parameter changes and
//! events, as VST3's objects. They're allocated once, when the instance is
//! made, so processing doesn't allocate.
//!
//! The host fills them before each block and reads them after, holding
//! the instance's busy flag, and the plugin reads and writes them as it
//! processes, within that. Each type here owns its object, and takes `&mut
//! self` to fill or read it, so only one side touches it at a time.

use std::cell::UnsafeCell;
use std::ptr;

use vst3::Steinberg::Vst::{
    Event, IEventList, IEventListTrait, IParamValueQueue, IParamValueQueueTrait, IParameterChanges,
    IParameterChangesTrait, ParamID, ParamValue,
};
use vst3::Steinberg::{int32, kInvalidArgument, kResultFalse, kResultOk, tresult};
use vst3::{Class, ComWrapper};

/// How many parameter changes a plugin can get in one block, beyond which
/// they're dropped and counted.
const CHANGE_CAPACITY: usize = 4096;
/// How many parameters can change in one block.
pub(crate) const QUEUE_CAPACITY: usize = 1024;
/// How many parameters a plugin can change in one block, and how many
/// points it can give each.
const OUTPUT_QUEUES: usize = 256;
const OUTPUT_POINTS: usize = 16;
/// How many events a plugin can get in one block, beyond which they're
/// dropped and counted.
pub(crate) const EVENT_CAPACITY: usize = 4096;

/// A parameter change, as it waits for the plugin.
#[derive(Debug, Clone, Copy)]
struct Point {
    id: ParamID,
    /// The order it came in, which it keeps among its parameter's points.
    order: u32,
    offset: i32,
    value: ParamValue,
    /// Whether the controller hears of it after the block, as it does of
    /// the graph's changes. It hears of the main thread's from the main
    /// thread.
    sync: bool,
}

/// The parameter changes a plugin gets in a block.
pub(crate) struct InputChanges {
    com: ComWrapper<InputList>,
}

struct InputList(UnsafeCell<Inputs>);

struct Inputs {
    /// Every change, as they came, then by parameter once ready.
    points: Vec<Point>,
    queues: Vec<ComWrapper<InputQueue>>,
    /// How many of `queues` hold this block's changes.
    used: usize,
    dropped: u32,
}

/// One parameter's points: a range of [`Inputs::points`].
struct InputQueue(UnsafeCell<(ParamID, *const Point, usize)>);

// SAFETY, for the four types below: only the host filling or reading them,
// or the plugin processing, touches them, never both at once (see the
// module's documentation). The pointers are into `points`, which doesn't
// move while the plugin reads it.
unsafe impl Send for InputList {}
// SAFETY: as above.
unsafe impl Sync for InputList {}
// SAFETY: as above.
unsafe impl Send for InputQueue {}
// SAFETY: as above.
unsafe impl Sync for InputQueue {}

impl InputChanges {
    pub(crate) fn new() -> Self {
        let queues = (0..QUEUE_CAPACITY)
            .map(|_| ComWrapper::new(InputQueue(UnsafeCell::new((0, ptr::null(), 0)))))
            .collect();
        Self {
            com: ComWrapper::new(InputList(UnsafeCell::new(Inputs {
                points: Vec::with_capacity(CHANGE_CAPACITY),
                queues,
                used: 0,
                dropped: 0,
            }))),
        }
    }

    fn inputs(&mut self) -> &mut Inputs {
        // SAFETY: `&mut self` is the host's turn: see the module's
        // documentation.
        unsafe { &mut *self.com.0.get() }
    }

    /// For the plugin's process call.
    pub(crate) fn as_ptr(&self) -> *mut IParameterChanges {
        self.com
            .as_com_ref::<IParameterChanges>()
            .map_or(ptr::null_mut(), |list| list.as_ptr())
    }

    pub(crate) fn clear(&mut self) {
        let inputs = self.inputs();
        inputs.points.clear();
        inputs.used = 0;
    }

    pub(crate) fn is_full(&mut self) -> bool {
        let inputs = self.inputs();
        inputs.points.len() == inputs.points.capacity()
    }

    pub(crate) fn is_empty(&mut self) -> bool {
        self.inputs().points.is_empty()
    }

    pub(crate) fn len(&mut self) -> usize {
        self.inputs().points.len()
    }

    /// Adds a change to `value`, from 0 to 1, at frame `offset`, and says
    /// whether there was room. `sync` tells the controller after the block.
    pub(crate) fn push(&mut self, id: ParamID, offset: i32, value: f64, sync: bool) -> bool {
        let inputs = self.inputs();
        if inputs.points.len() == inputs.points.capacity() || !value.is_finite() {
            inputs.dropped += 1;
            return false;
        }
        let order = inputs.points.len() as u32;
        inputs.points.push(Point {
            id,
            order,
            offset,
            value: value.clamp(0.0, 1.0),
            sync,
        });
        true
    }

    /// Puts the changes in queues, one per parameter, for the plugin.
    /// Parameters past the queues the plugin can have are dropped.
    pub(crate) fn ready(&mut self) {
        let inputs = self.inputs();
        // Unstable, but the order they came in sorts them as they came.
        inputs.points.sort_unstable_by_key(|p| (p.id, p.order));
        let mut used = 0;
        let mut start = 0;
        let points = inputs.points.as_ptr();
        while start < inputs.points.len() {
            let id = inputs.points[start].id;
            let len = inputs.points[start..]
                .iter()
                .take_while(|p| p.id == id)
                .count();
            let Some(queue) = inputs.queues.get(used) else {
                inputs.dropped += (inputs.points.len() - start) as u32;
                inputs.points.truncate(start);
                break;
            };
            // SAFETY: the host's turn, and the plugin has no queue now.
            unsafe { *queue.0.get() = (id, points.wrapping_add(start), len) };
            used += 1;
            start += len;
        }
        inputs.used = used;
    }

    /// Calls `f` with the last value of each parameter whose change the
    /// controller is to hear of, after the block.
    pub(crate) fn synced(&mut self, mut f: impl FnMut(ParamID, f64)) {
        for points in self.inputs().points.chunk_by(|a, b| a.id == b.id) {
            if let Some(last) = points.last()
                && last.sync
            {
                f(last.id, last.value);
            }
        }
    }

    /// Changes dropped since the plugin was activated.
    #[cfg(test)]
    pub(crate) fn dropped(&mut self) -> u32 {
        self.inputs().dropped
    }
}

impl Class for InputList {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for InputList {
    unsafe fn getParameterCount(&self) -> int32 {
        // SAFETY: the plugin's turn.
        unsafe { (*self.0.get()).used as int32 }
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        // SAFETY: the plugin's turn.
        let inputs = unsafe { &*self.0.get() };
        usize::try_from(index)
            .ok()
            .filter(|&i| i < inputs.used)
            .and_then(|i| inputs.queues[i].as_com_ref::<IParamValueQueue>())
            .map_or(ptr::null_mut(), |queue| queue.as_ptr())
    }

    /// The host's changes are the host's.
    unsafe fn addParameterData(
        &self,
        _: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if !index.is_null() {
            // SAFETY: the plugin passes somewhere to write.
            unsafe { index.write(-1) };
        }
        ptr::null_mut()
    }
}

impl Class for InputQueue {
    type Interfaces = (IParamValueQueue,);
}

impl InputQueue {
    fn points(&self) -> (ParamID, &[Point]) {
        // SAFETY: the plugin's turn; the range is in `points`, as `ready`
        // set it.
        unsafe {
            let (id, points, len) = *self.0.get();
            if points.is_null() {
                (id, &[])
            } else {
                (id, std::slice::from_raw_parts(points, len))
            }
        }
    }
}

impl IParamValueQueueTrait for InputQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.points().0
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points().1.len() as int32
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let points = self.points().1;
        let Some(point) = usize::try_from(index).ok().and_then(|i| points.get(i)) else {
            return kInvalidArgument;
        };
        if offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the plugin passes somewhere to write.
        unsafe {
            offset.write(point.offset);
            value.write(point.value);
        }
        kResultOk
    }

    unsafe fn addPoint(&self, _: int32, _: ParamValue, index: *mut int32) -> tresult {
        if !index.is_null() {
            // SAFETY: the plugin passes somewhere to write.
            unsafe { index.write(-1) };
        }
        kResultFalse
    }
}

/// The parameter changes a plugin makes as it processes, as of a meter, or
/// a value it changes itself.
pub(crate) struct OutputChanges {
    com: ComWrapper<OutputList>,
}

struct OutputList(UnsafeCell<Outputs>);

struct Outputs {
    queues: Vec<ComWrapper<OutputQueue>>,
    used: usize,
}

/// One parameter's points, as the plugin adds them.
struct OutputQueue(UnsafeCell<(ParamID, Vec<(i32, ParamValue)>)>);

// SAFETY, for the four types below: as for `InputList`.
unsafe impl Send for OutputList {}
// SAFETY: as above.
unsafe impl Sync for OutputList {}
// SAFETY: as above.
unsafe impl Send for OutputQueue {}
// SAFETY: as above.
unsafe impl Sync for OutputQueue {}

impl OutputChanges {
    pub(crate) fn new() -> Self {
        let queues = (0..OUTPUT_QUEUES)
            .map(|_| {
                let points = Vec::with_capacity(OUTPUT_POINTS);
                ComWrapper::new(OutputQueue(UnsafeCell::new((0, points))))
            })
            .collect();
        Self {
            com: ComWrapper::new(OutputList(UnsafeCell::new(Outputs { queues, used: 0 }))),
        }
    }

    fn outputs(&mut self) -> &mut Outputs {
        // SAFETY: `&mut self` is the host's turn.
        unsafe { &mut *self.com.0.get() }
    }

    pub(crate) fn as_ptr(&self) -> *mut IParameterChanges {
        self.com
            .as_com_ref::<IParameterChanges>()
            .map_or(ptr::null_mut(), |list| list.as_ptr())
    }

    pub(crate) fn clear(&mut self) {
        self.outputs().used = 0;
    }

    /// Calls `f` with the last value the plugin gave each parameter.
    pub(crate) fn last_values(&mut self, mut f: impl FnMut(ParamID, f64)) {
        let outputs = self.outputs();
        for queue in &outputs.queues[..outputs.used] {
            // SAFETY: the host's turn.
            let (id, points) = unsafe { &*queue.0.get() };
            if let Some(&(_, value)) = points.last() {
                f(*id, value);
            }
        }
    }
}

impl Class for OutputList {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for OutputList {
    unsafe fn getParameterCount(&self) -> int32 {
        // SAFETY: the plugin's turn.
        unsafe { (*self.0.get()).used as int32 }
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        // SAFETY: the plugin's turn.
        let outputs = unsafe { &*self.0.get() };
        usize::try_from(index)
            .ok()
            .filter(|&i| i < outputs.used)
            .and_then(|i| outputs.queues[i].as_com_ref::<IParamValueQueue>())
            .map_or(ptr::null_mut(), |queue| queue.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: the plugin's turn, and it passes an id.
        let (outputs, id) = unsafe { (&mut *self.0.get(), *id) };
        let found = outputs.queues[..outputs.used]
            .iter()
            // SAFETY: as above.
            .position(|queue| unsafe { (*queue.0.get()).0 } == id);
        let at = match found {
            Some(at) => at,
            None if outputs.used < outputs.queues.len() => {
                let at = outputs.used;
                // SAFETY: as above.
                let queue = unsafe { &mut *outputs.queues[at].0.get() };
                queue.0 = id;
                queue.1.clear();
                outputs.used += 1;
                at
            }
            None => return ptr::null_mut(),
        };
        if !index.is_null() {
            // SAFETY: the plugin passes somewhere to write.
            unsafe { index.write(at as int32) };
        }
        outputs.queues[at]
            .as_com_ref::<IParamValueQueue>()
            .map_or(ptr::null_mut(), |queue| queue.as_ptr())
    }
}

impl Class for OutputQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for OutputQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        // SAFETY: the plugin's turn.
        unsafe { (*self.0.get()).0 }
    }

    unsafe fn getPointCount(&self) -> int32 {
        // SAFETY: the plugin's turn.
        unsafe { (*self.0.get()).1.len() as int32 }
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        // SAFETY: the plugin's turn.
        let points = unsafe { &(*self.0.get()).1 };
        let Some(&(at, v)) = usize::try_from(index).ok().and_then(|i| points.get(i)) else {
            return kInvalidArgument;
        };
        if offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the plugin passes somewhere to write.
        unsafe {
            offset.write(at);
            value.write(v);
        }
        kResultOk
    }

    /// A queue that's full keeps the last value the plugin gives it.
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        // SAFETY: the plugin's turn.
        let points = unsafe { &mut (*self.0.get()).1 };
        if points.len() == points.capacity() {
            points.pop();
        }
        points.push((offset, value));
        if !index.is_null() {
            // SAFETY: the plugin passes somewhere to write.
            unsafe { index.write(points.len() as int32 - 1) };
        }
        kResultOk
    }
}

/// A block's events: the notes for the plugin, or those the plugin plays,
/// as many as there's room for.
pub(crate) struct Events {
    com: ComWrapper<EventList>,
}

struct EventList {
    events: UnsafeCell<Vec<Event>>,
    /// Whether it's the plugin's, which keeps what the plugin adds.
    output: bool,
}

// SAFETY: as for `InputList`.
unsafe impl Send for EventList {}
// SAFETY: as above.
unsafe impl Sync for EventList {}

impl Events {
    /// A list for `capacity` events, or for the plugin's output.
    pub(crate) fn new(capacity: usize, output: bool) -> Self {
        Self {
            com: ComWrapper::new(EventList {
                events: UnsafeCell::new(Vec::with_capacity(capacity)),
                output,
            }),
        }
    }

    fn events(&mut self) -> &mut Vec<Event> {
        // SAFETY: `&mut self` is the host's turn.
        unsafe { &mut *self.com.events.get() }
    }

    pub(crate) fn as_ptr(&self) -> *mut IEventList {
        self.com
            .as_com_ref::<IEventList>()
            .map_or(ptr::null_mut(), |list| list.as_ptr())
    }

    pub(crate) fn clear(&mut self) {
        self.events().clear();
    }

    /// The events in it, in the order they came.
    pub(crate) fn list(&mut self) -> &[Event] {
        self.events()
    }

    /// Adds an event, unless the list is full: says whether it did.
    pub(crate) fn push(&mut self, event: Event) -> bool {
        let events = self.events();
        if events.len() == events.capacity() {
            return false;
        }
        events.push(event);
        true
    }
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        // SAFETY: the plugin's turn.
        unsafe { (*self.events.get()).len() as int32 }
    }

    unsafe fn getEvent(&self, index: int32, event: *mut Event) -> tresult {
        // SAFETY: the plugin's turn.
        let events = unsafe { &*self.events.get() };
        let Some(found) = usize::try_from(index).ok().and_then(|i| events.get(i)) else {
            return kInvalidArgument;
        };
        if event.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the plugin passes somewhere to write.
        unsafe { event.write(*found) };
        kResultOk
    }

    unsafe fn addEvent(&self, event: *mut Event) -> tresult {
        // SAFETY: the plugin's turn.
        let events = unsafe { &mut *self.events.get() };
        // SAFETY: the plugin passes its event, or null.
        match (self.output, unsafe { event.as_ref() }) {
            (true, Some(event)) if events.len() < events.capacity() => {
                events.push(*event);
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

#[cfg(test)]
mod tests {
    use vst3::ComRef;

    use super::*;

    /// Each parameter's points, as the plugin reads them from `list`.
    fn read(list: *mut IParameterChanges) -> Vec<(ParamID, Vec<(i32, f64)>)> {
        // SAFETY: the list is alive, and it's the plugin's turn: every call.
        unsafe {
            let list = ComRef::from_raw(list).unwrap();
            (0..list.getParameterCount())
                .map(|i| {
                    let queue = ComRef::from_raw(list.getParameterData(i)).unwrap();
                    let points = (0..queue.getPointCount())
                        .map(|p| {
                            let (mut offset, mut value) = (0, 0.0);
                            assert_eq!(queue.getPoint(p, &mut offset, &mut value), kResultOk);
                            (offset, value)
                        })
                        .collect();
                    (queue.getParameterId(), points)
                })
                .collect()
        }
    }

    #[test]
    fn changes_reach_the_plugin_by_parameter_in_order() {
        let mut changes = InputChanges::new();
        changes.push(7, 0, 0.5, false);
        changes.push(3, 10, 0.25, true);
        changes.push(7, 20, 0.75, true);
        changes.push(3, 30, 2.0, false);
        assert!(!changes.push(5, 0, f64::NAN, false));
        changes.ready();
        assert_eq!(
            read(changes.as_ptr()),
            [
                (3, vec![(10, 0.25), (30, 1.0)]),
                (7, vec![(0, 0.5), (20, 0.75)])
            ]
        );
        // The controller hears of a parameter's last change, if it's one to
        // hear of.
        let mut synced = Vec::new();
        changes.synced(|id, value| synced.push((id, value)));
        assert_eq!(synced, [(7, 0.75)]);
        changes.clear();
        changes.ready();
        assert!(read(changes.as_ptr()).is_empty());
    }

    #[test]
    fn changes_past_what_a_block_holds_are_dropped_and_counted() {
        let mut changes = InputChanges::new();
        // One parameter more than there are queues, the last with 3 points.
        let params = QUEUE_CAPACITY as u32 + 1;
        for i in 0..CHANGE_CAPACITY as u32 {
            assert!(changes.push(i % params, 0, 0.5, false));
        }
        assert!(changes.is_full());
        assert!(!changes.push(0, 0, 0.5, false));
        changes.ready();
        let read = read(changes.as_ptr());
        assert_eq!(read.len(), QUEUE_CAPACITY);
        assert_eq!(read.last().unwrap().0, params - 2);
        assert_eq!(changes.dropped(), 1 + 3);
    }

    #[test]
    fn the_plugins_changes_come_back_as_last_values() {
        let mut out = OutputChanges::new();
        // SAFETY: the list is alive, and it's the plugin's turn.
        unsafe {
            let list = ComRef::from_raw(out.as_ptr()).unwrap();
            let mut index = -1;
            let queue = ComRef::from_raw(list.addParameterData(&12, &mut index)).unwrap();
            assert_eq!(index, 0);
            // More points than a queue holds: it keeps the last.
            for i in 0..20 {
                assert_eq!(
                    queue.addPoint(i, f64::from(i) / 20.0, &mut index),
                    kResultOk
                );
            }
            assert_eq!(queue.getPointCount(), OUTPUT_POINTS as i32);
            list.addParameterData(&12, &mut index);
            assert_eq!(index, 0);
            let other = ComRef::from_raw(list.addParameterData(&13, &mut index)).unwrap();
            assert_eq!(index, 1);
            other.addPoint(0, 0.5, &mut index);
            assert_eq!(list.getParameterCount(), 2);
        }
        let mut values = Vec::new();
        out.last_values(|id, value| values.push((id, value)));
        assert_eq!(values, [(12, 19.0 / 20.0), (13, 0.5)]);
        out.clear();
        out.last_values(|_, _| panic!("cleared"));
    }

    #[test]
    fn events_reach_the_plugin_and_its_own_come_back() {
        // SAFETY: zeroed is a valid event.
        let event: Event = unsafe { std::mem::zeroed() };
        let mut events = Events::new(2, false);
        assert!(events.push(Event {
            sampleOffset: 1,
            ..event
        }));
        assert!(events.push(Event {
            sampleOffset: 2,
            ..event
        }));
        assert!(!events.push(event));
        let mut output = Events::new(1, true);
        // SAFETY: the lists are alive, and it's the plugin's turn.
        unsafe {
            let list = ComRef::from_raw(events.as_ptr()).unwrap();
            assert_eq!(list.getEventCount(), 2);
            let mut got = event;
            assert_eq!(list.getEvent(1, &mut got), kResultOk);
            assert_eq!(got.sampleOffset, 2);
            assert_eq!(list.getEvent(2, &mut got), kInvalidArgument);
            assert_eq!(list.addEvent(&mut got), kResultFalse);
            // The plugin's own, as many as there's room for.
            let list = ComRef::from_raw(output.as_ptr()).unwrap();
            assert_eq!(list.addEvent(&mut got), kResultOk);
            assert_eq!(list.addEvent(&mut got), kResultFalse);
            assert_eq!(list.addEvent(ptr::null_mut()), kResultFalse);
        }
        let offsets: Vec<i32> = output.list().iter().map(|e| e.sampleOffset).collect();
        assert_eq!(offsets, [2]);
        output.clear();
        assert!(output.list().is_empty());
    }
}
