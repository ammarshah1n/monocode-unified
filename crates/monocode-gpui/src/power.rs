//! The app's power budget for decorative motion and background polling.
//!
//! macOS Low Power Mode is read once at start and then follows the system's
//! change notification; nothing polls for it. While it is on, loops on the
//! shared animation clock run at half rate and the purely decorative
//! pixel-art loops hold their resting frame (see `pulse`). Background
//! pollers stretch their interval through `poll_interval`.
use gpui::{App, Global};
use std::time::Duration;

#[derive(Default)]
struct Budget {
    low_power: bool,
}
impl Global for Budget {}

/// How much slower an inactive app's pollers run.
const INACTIVE_POLL_STRIDE: u32 = 4;

/// Whether the system asked apps to save power.
pub(crate) fn low_power(cx: &App) -> bool {
    cx.try_global::<Budget>().is_some_and(|budget| budget.low_power)
}

/// How many times slower decorative loops run on the shared clock.
pub(crate) fn stride_multiplier(cx: &App) -> u64 {
    if low_power(cx) { 2 } else { 1 }
}

/// The wait before a poller's next pass: `base` while the app is in use,
/// longer while it is in the background or the system is saving power.
pub(crate) fn poll_interval(base: Duration, active: bool, cx: &App) -> Duration {
    scaled(base, active, low_power(cx))
}

fn scaled(base: Duration, active: bool, low_power: bool) -> Duration {
    let mut interval = base;
    if !active {
        interval *= INACTIVE_POLL_STRIDE;
    }
    if low_power {
        interval *= 2;
    }
    interval
}

/// Records the system's state and repaints, so loops pick the new budget up
/// (a loop that held its resting frame starts again when power returns).
pub(crate) fn set_low_power(cx: &mut App, low_power: bool) {
    if self::low_power(cx) != low_power {
        cx.default_global::<Budget>().low_power = low_power;
        cx.refresh_windows();
    }
}

pub(crate) fn init(cx: &mut App) {
    cx.set_global(Budget::default());
    #[cfg(target_os = "macos")]
    macos::watch(cx);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::RcBlock;
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSProcessInfo,
        NSProcessInfoPowerStateDidChangeNotification,
    };
    use std::ptr::NonNull;

    fn enabled() -> bool {
        NSProcessInfo::processInfo().isLowPowerModeEnabled()
    }

    pub(super) fn watch(cx: &mut App) {
        set_low_power(cx, enabled());
        // The notification may arrive on any thread: it only signals, and the
        // app thread reads the state. A full channel already holds a signal.
        let (signal, changes) = async_channel::bounded::<()>(1);
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let _ = signal.try_send(());
        });
        // SAFETY: the block only sends on a thread-safe channel; with no queue
        // it runs on the posting thread, which that allows.
        let observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSProcessInfoPowerStateDidChangeNotification),
                None,
                None,
                &block,
            )
        };
        cx.spawn(async move |cx| {
            // The centre keeps the observer while this task holds it.
            let _observer = observer;
            while changes.recv().await.is_ok() {
                cx.update(|cx| set_low_power(cx, enabled()));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pollers_stretch_when_inactive_or_saving_power() {
        let base = Duration::from_secs(3);
        assert_eq!(scaled(base, true, false), base);
        assert_eq!(scaled(base, false, false), base * 4);
        assert_eq!(scaled(base, true, true), base * 2);
        assert_eq!(scaled(base, false, true), base * 8);
    }

    #[gpui::test]
    fn low_power_is_off_until_the_system_says_otherwise(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(!low_power(cx));
            assert_eq!(stride_multiplier(cx), 1);
            set_low_power(cx, true);
            assert!(low_power(cx));
            assert_eq!(stride_multiplier(cx), 2);
            assert_eq!(
                poll_interval(Duration::from_secs(2), true, cx),
                Duration::from_secs(4)
            );
            set_low_power(cx, false);
            assert!(!low_power(cx));
        });
    }
}
