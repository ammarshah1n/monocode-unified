//! One shared clock for decorative loops: spinners, mascots, avatars and the
//! composer runner.
//!
//! A view that paints a loop leases the clock for itself. Each tick notifies
//! every leased view together, so all loops land in one frame instead of one
//! frame per spinner, and at 30Hz or less instead of the display's 120Hz. A
//! lease ends when its view was notified but did not paint a loop again, so
//! a closed, scrolled-away or hidden spinner stops costing anything, and the
//! clock parks once no lease is left.
//!
//! The timer sleeps until the next lease is due rather than waking every
//! frame, so Half and Low loops wake at their own pace. While no window is
//! active the clock slows every loop down further and parks purely decorative
//! ones outright (the window repaints on activation, which leases them again);
//! only loops that step state, like stream presentation, keep running. Low
//! Power Mode halves every rate (see `power`).
use crate::power;
use gpui::{
    AnyElement, App, Element, ElementId, EntityId, Global, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Task, Window,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const TICK: Duration = Duration::from_millis(33);
/// An inactive app's loops tick this many times slower.
const BACKGROUND_STRIDE: u64 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rate {
    /// About 30fps: rotations and fades that must look continuous.
    Smooth,
    /// About 15fps: small loaders.
    Half,
    /// About 7.5fps: pixel art that steps between a few poses.
    Low,
}
impl Rate {
    fn stride(self) -> u64 {
        match self {
            Rate::Smooth => 1,
            Rate::Half => 2,
            Rate::Low => 4,
        }
    }
}

type FrameCallback = Box<dyn FnOnce(&mut App)>;
struct Lease {
    rate: Rate,
    /// Painted a loop since the clock last notified it.
    painted: bool,
    /// Also steps state on the clock, so it keeps running with no active window.
    stepping: bool,
    /// The tick this lease started or was last serviced on.
    seen: u64,
    on_tick: Option<FrameCallback>,
}
impl Lease {
    fn stride(&self, background: bool, slow: u64) -> u64 {
        self.rate.stride() * if background { BACKGROUND_STRIDE } else { 1 } * slow
    }
    /// The first tick after `seen` this lease is serviced on. Ticks are
    /// multiples of the stride, so loops of one rate land in one frame.
    fn next(&self, background: bool, slow: u64) -> u64 {
        let stride = self.stride(background, slow);
        (self.seen / stride + 1) * stride
    }
}

#[derive(Default)]
struct Clock {
    epoch: Option<Instant>,
    leases: HashMap<EntityId, Lease>,
    /// Waiters for the next tick, see [`beat`].
    beats: Vec<async_channel::Sender<()>>,
    running: bool,
    /// No window was active when the clock last ticked.
    background: bool,
    /// The tick the pending timer fires on.
    wake: u64,
    timer: Option<Task<()>>,
}
impl Global for Clock {}
impl Clock {
    fn tick_at(&mut self, now: Instant) -> u64 {
        let epoch = *self.epoch.get_or_insert(now);
        (now.saturating_duration_since(epoch).as_nanos() / TICK.as_nanos()) as u64
    }
    fn wait_until(&self, tick: u64, now: Instant) -> Duration {
        let epoch = self.epoch.unwrap_or(now);
        (epoch + Duration::from_nanos(TICK.as_nanos() as u64 * tick)).saturating_duration_since(now)
    }
}

/// Keeps the view being drawn repainting at `rate`. Call it while rendering
/// or painting a loop, every time the loop is drawn. Returns false, and
/// leases nothing, under reduced motion: draw the loop's resting frame.
pub(crate) fn request(rate: Rate, window: &Window, cx: &mut App) -> bool {
    request_inner(rate, window, cx, None)
}

/// Stream presentation uses the same coalesced 30Hz wakeup as decorative
/// loops, advancing its state before notifying rather than spawning a timer.
pub(crate) fn request_frame(
    rate: Rate,
    window: &Window,
    cx: &mut App,
    on_tick: impl FnOnce(&mut App) + 'static,
) -> bool {
    request_inner(rate, window, cx, Some(Box::new(on_tick)))
}

fn request_inner(
    rate: Rate,
    window: &Window,
    cx: &mut App,
    mut on_tick: Option<FrameCallback>,
) -> bool {
    // Low Power Mode holds the purely decorative pixel-art loops still.
    if cx.reduce_motion() || (rate == Rate::Low && on_tick.is_none() && power::low_power(cx)) {
        return false;
    }
    let view = window.current_view();
    let now = cx.background_executor().now();
    let slow = power::stride_multiplier(cx);
    let active = window.is_window_active();
    let stepping = on_tick.is_some();
    let clock = cx.default_global::<Clock>();
    let at = clock.tick_at(now);
    // Another window may be active; the last tick knows.
    let background = clock.background && !active;
    let lease = clock
        .leases
        .entry(view)
        .and_modify(|lease| {
            lease.painted = true;
            if on_tick.is_some() {
                lease.on_tick = on_tick.take();
                lease.stepping = true;
            }
            if lease.rate.stride() > rate.stride() {
                lease.rate = rate;
            }
        })
        .or_insert_with(|| Lease {
            rate,
            painted: true,
            stepping,
            seen: at,
            on_tick,
        });
    let next = lease.next(background, slow);
    // A new or faster lease may be due before the timer's pending wakeup.
    if !clock.running || next < clock.wake {
        clock.running = true;
        clock.wake = next;
        let wait = clock.wait_until(next, now);
        let timer = cx.spawn(async move |cx| {
            let mut wait = wait;
            loop {
                cx.background_executor().timer(wait).await;
                match cx.update(tick) {
                    Some(next) => wait = next,
                    None => break,
                }
            }
        });
        cx.global_mut::<Clock>().timer = Some(timer);
    }
    true
}

/// Time on the shared clock, so loops of one period stay in step.
pub(crate) fn elapsed(cx: &mut App) -> Duration {
    let now = cx.background_executor().now();
    let clock = cx.default_global::<Clock>();
    now - *clock.epoch.get_or_insert(now)
}

/// The fraction of `period` the shared clock is into, in `0..1`.
pub(crate) fn phase(period: Duration, cx: &mut App) -> f32 {
    if period.is_zero() {
        return 0.;
    }
    let into = elapsed(cx).as_nanos() % period.as_nanos();
    into as f32 / period.as_nanos() as f32
}

#[cfg(test)]
pub(crate) fn running(cx: &App) -> bool {
    cx.try_global::<Clock>().is_some_and(|clock| clock.running)
}

/// Waits for the clock's next tick, or returns at once while no loop runs.
/// Work that would otherwise draw a frame of its own, such as a streamed
/// reply's updates, lands in the loops' frame instead.
pub(crate) async fn beat(cx: &mut gpui::AsyncApp) {
    // Tests park the executor without advancing the clock, so a tick would
    // never come; they apply updates as soon as they arrive.
    if cfg!(test) {
        return;
    }
    let beat = cx.update(|cx| {
        let clock = cx.default_global::<Clock>();
        clock.running.then(|| {
            let (tx, rx) = async_channel::bounded(1);
            clock.beats.push(tx);
            rx
        })
    });
    if let Some(beat) = beat {
        beat.recv().await.ok();
    }
}

/// Services the leases due now and returns how long to sleep until the next
/// one is, or `None` once none is left.
fn tick(cx: &mut App) -> Option<Duration> {
    let now = cx.background_executor().now();
    let (visible, active) = window_state(cx);
    let slow = power::stride_multiplier(cx);
    let background = !active;
    let clock = cx.global_mut::<Clock>();
    // Before the loops notify, so whatever the waiters change draws with them.
    for beat in clock.beats.drain(..) {
        beat.try_send(()).ok();
    }
    clock.background = background;
    let tick = clock.tick_at(now);
    let mut due = Vec::new();
    clock.leases.retain(|view, lease| {
        if tick < lease.next(background, slow) {
            return true;
        }
        if !lease.painted {
            return false;
        }
        lease.painted = false;
        lease.seen = tick;
        if !lease.stepping {
            // Hidden: mark the view for its next frame, which leases it again
            // when the window returns. Inactive: activation repaints it.
            if !visible {
                due.push((*view, None));
                return false;
            }
            if background {
                return false;
            }
        }
        due.push((*view, lease.on_tick.take()));
        true
    });
    let next = clock
        .leases
        .values()
        .map(|lease| lease.next(background, slow))
        .min();
    clock.running = next.is_some();
    let wait = next.map(|next| {
        clock.wake = next;
        clock.wait_until(next, now)
    });
    for (view, callback) in due {
        if let Some(callback) = callback {
            callback(cx);
        } else {
            // A loop's frames keep its size; views can keep their measurements.
            cx.notify_paint(view);
        }
    }
    wait
}

/// Whether one of the app's windows has focus.
pub(crate) fn app_active(cx: &mut App) -> bool {
    window_state(cx).1
}

/// Whether any window is presented, and whether any is the active one.
fn window_state(cx: &mut App) -> (bool, bool) {
    let mut state = (false, false);
    for window in cx.windows() {
        if let Ok((visible, active)) = window.update(cx, |_, window, _| {
            (window.is_visible(), window.is_window_active())
        }) {
            state.0 |= visible;
            state.1 |= active;
        }
    }
    state
}

/// A repeating animation on the shared clock: `build` gets the fraction of
/// `period` elapsed and returns the frame. Reduced motion draws phase zero.
pub(crate) fn looping<E: IntoElement>(
    id: impl Into<ElementId>,
    period: Duration,
    rate: Rate,
    build: impl FnOnce(f32) -> E + 'static,
) -> Looping {
    Looping {
        id: id.into(),
        period,
        rate,
        build: Some(Box::new(move |phase| build(phase).into_any_element())),
    }
}

pub(crate) struct Looping {
    id: ElementId,
    period: Duration,
    rate: Rate,
    build: Option<Box<dyn FnOnce(f32) -> AnyElement>>,
}
impl IntoElement for Looping {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Looping {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let phase = if request(self.rate, window, cx) {
            phase(self.period, cx)
        } else {
            0.
        };
        let build = self.build.take().expect("a loop lays out once");
        let mut element = build(phase);
        (element.request_layout(window, cx), element)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, VisualTestContext, div, prelude::*};
    use std::{cell::Cell, rc::Rc};

    struct Spinner {
        spinning: bool,
        rate: Rate,
        renders: Rc<Cell<usize>>,
    }
    impl Render for Spinner {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let (spinning, rate) = (self.spinning, self.rate);
            div().size_4().when(spinning, |d| {
                d.child(looping("spin", Duration::from_secs(1), rate, |_| div()))
            })
        }
    }
    fn spinner(
        rate: Rate,
        active: bool,
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<Spinner>, &mut VisualTestContext, Rc<Cell<usize>>) {
        cx.update(|cx| cx.set_reduce_motion(false));
        let renders = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, _| Spinner {
            spinning: true,
            rate,
            renders: renders.clone(),
        });
        // A test window starts inactive.
        if active {
            cx.update(|window, _| window.activate_window());
        }
        cx.run_until_parked();
        (view, cx, renders)
    }
    fn advance(cx: &mut VisualTestContext, ticks: u32) {
        cx.executor().advance_clock(TICK * ticks);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_loop_repaints_on_the_clock_and_parks_once_it_is_gone(cx: &mut TestAppContext) {
        let (view, cx, renders) = spinner(Rate::Smooth, true, cx);
        let first = renders.get();
        advance(cx, 2);
        assert!(renders.get() > first, "a leased view repaints on ticks");

        view.update(cx, |view, cx| {
            view.spinning = false;
            cx.notify();
        });
        cx.run_until_parked();
        advance(cx, 3);
        let stopped = renders.get();
        advance(cx, 3);
        assert_eq!(
            renders.get(),
            stopped,
            "an unpainted loop releases the clock"
        );
        cx.update(|_, cx| assert!(!cx.global::<Clock>().running));
    }

    #[gpui::test]
    fn the_timer_sleeps_between_slow_loops(cx: &mut TestAppContext) {
        // A Low loop is due every fourth tick: the wakeups between are gone.
        let (_view, cx, renders) = spinner(Rate::Low, true, cx);
        let first = renders.get();
        advance(cx, 3);
        assert_eq!(renders.get(), first, "not due before its stride");
        cx.update(|_, cx| {
            let clock = cx.global::<Clock>();
            assert!(clock.running);
            assert_eq!(clock.wake, Rate::Low.stride());
        });
        advance(cx, 1);
        assert!(renders.get() > first, "repaints on its own tick");
    }

    #[gpui::test]
    fn an_inactive_window_parks_decorative_loops_without_repainting(cx: &mut TestAppContext) {
        let (_view, cx, renders) = spinner(Rate::Smooth, false, cx);
        let first = renders.get();
        advance(cx, BACKGROUND_STRIDE as u32 * 3);
        assert_eq!(renders.get(), first, "nothing repaints in the background");
        cx.update(|_, cx| assert!(!cx.global::<Clock>().running, "the timer stops"));
    }

    #[gpui::test]
    fn low_power_halves_the_rate_and_holds_pixel_art_still(cx: &mut TestAppContext) {
        let (_view, cx, renders) = spinner(Rate::Smooth, true, cx);
        cx.update(|_, cx| power::set_low_power(cx, true));
        cx.run_until_parked();
        let first = renders.get();
        advance(cx, 8);
        let ticks = renders.get() - first;
        assert!(
            (3..=5).contains(&ticks),
            "a 30fps loop runs at about half rate in Low Power Mode, got {ticks} repaints in 8 ticks"
        );
    }

    #[gpui::test]
    fn pixel_art_leases_nothing_in_low_power_mode(cx: &mut TestAppContext) {
        cx.update(|cx| power::set_low_power(cx, true));
        let (_view, cx, renders) = spinner(Rate::Low, true, cx);
        let first = renders.get();
        advance(cx, 16);
        assert_eq!(renders.get(), first, "decorative pixel art holds still");
        cx.update(|_, cx| assert!(!running(cx)));
    }

    #[gpui::test]
    fn reduced_motion_leases_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let renders = Rc::new(Cell::new(0));
        let (_view, cx) = cx.add_window_view(|_, _| Spinner {
            spinning: true,
            rate: Rate::Smooth,
            renders: renders.clone(),
        });
        cx.run_until_parked();
        let first = renders.get();
        advance(cx, BACKGROUND_STRIDE as u32 * 3);
        assert_eq!(renders.get(), first);
    }

    #[gpui::test]
    fn stream_callbacks_share_the_clock_and_release_their_lease(cx: &mut TestAppContext) {
        struct Stream {
            active: bool,
            ticks: Rc<Cell<usize>>,
        }
        impl Render for Stream {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let active = self.active;
                let view = cx.entity().downgrade();
                div().size_4().child(gpui::canvas(
                    move |_, window, cx| {
                        if active {
                            request_frame(Rate::Smooth, window, cx, move |cx| {
                                let _ = view.update(cx, |stream, cx| {
                                    if stream.active {
                                        stream.ticks.set(stream.ticks.get() + 1);
                                        cx.notify();
                                    }
                                });
                            });
                        }
                    },
                    |_, _, _, _| {},
                ))
            }
        }
        cx.update(|cx| cx.set_reduce_motion(false));
        let ticks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view(|_, _| Stream {
            active: true,
            ticks: ticks.clone(),
        });
        cx.run_until_parked();
        cx.executor().advance_clock(TICK * BACKGROUND_STRIDE as u32);
        cx.run_until_parked();
        assert_eq!(ticks.get(), 1);
        view.update(cx, |stream, cx| {
            stream.active = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor()
            .advance_clock(TICK * BACKGROUND_STRIDE as u32 * 3);
        cx.run_until_parked();
        assert_eq!(ticks.get(), 1);
        cx.update(|_, cx| assert!(!running(cx)));
    }
}
