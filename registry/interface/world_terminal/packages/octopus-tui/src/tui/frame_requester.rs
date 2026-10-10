use std::time::Duration;
use std::time::Instant;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use super::frame_rate_limiter::FrameRateLimiter;

#[derive(Clone, Debug)]
pub struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let scheduler = FrameScheduler::new(rx, draw_tx);
        tokio::spawn(scheduler.run());
        Self {
            frame_schedule_tx: tx,
        }
    }

    pub fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }

    pub fn schedule_frame_in(&self, dur: Duration) {
        let _ = self.frame_schedule_tx.send(Instant::now() + dur);
    }
}

#[cfg(test)]
impl FrameRequester {
    pub(crate) fn test_dummy() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        FrameRequester {
            frame_schedule_tx: tx,
        }
    }
}

struct FrameScheduler {
    receiver: mpsc::UnboundedReceiver<Instant>,
    draw_tx: broadcast::Sender<()>,
    rate_limiter: FrameRateLimiter,
}

impl FrameScheduler {
    fn new(receiver: mpsc::UnboundedReceiver<Instant>, draw_tx: broadcast::Sender<()>) -> Self {
        Self {
            receiver,
            draw_tx,
            rate_limiter: FrameRateLimiter::default(),
        }
    }

    async fn run(mut self) {
        const ONE_YEAR: Duration = Duration::from_secs(60 * 60 * 24 * 365);
        let mut next_deadline: Option<Instant> = None;
        loop {
            let target = next_deadline.unwrap_or_else(|| Instant::now() + ONE_YEAR);
            let deadline = tokio::time::sleep_until(target.into());
            tokio::pin!(deadline);

            tokio::select! {
                draw_at = self.receiver.recv() => {
                    let Some(draw_at) = draw_at else {
                        break
                    };
                    let draw_at = self.rate_limiter.clamp_deadline(draw_at);
                    next_deadline = Some(next_deadline.map_or(draw_at, |cur| cur.min(draw_at)));

                    continue;
                }
                _ = &mut deadline => {
                    if next_deadline.is_some() {
                        next_deadline = None;
                        self.rate_limiter.mark_emitted(target);
                        let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::super::frame_rate_limiter::MIN_FRAME_INTERVAL;
    use super::*;
    use tokio::time;
    use tokio_util::time::FutureExt;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_schedule_frame_immediate_triggers_once() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();

        time::advance(Duration::from_millis(1)).await;

        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_schedule_frame_in_triggers_at_delay() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame_in(Duration::from_millis(50));

        time::advance(Duration::from_millis(30)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
        assert!(early.is_err(), "draw fired too early");

        time::advance(Duration::from_millis(25)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for scheduled draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_coalesces_multiple_requests_into_single_draw() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        requester.schedule_frame();
        requester.schedule_frame();

        time::advance(Duration::from_millis(1)).await;

        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for coalesced draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_coalesces_mixed_immediate_and_delayed_requests() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame_in(Duration::from_millis(100));
        requester.schedule_frame();

        time::advance(Duration::from_millis(1)).await;

        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for coalesced immediate draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_limits_draw_notifications_to_120fps() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(
            early.is_err(),
            "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
        );

        time::advance(MIN_FRAME_INTERVAL).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for second draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_rate_limit_clamps_early_delayed_requests() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame_in(Duration::from_millis(1));

        time::advance(MIN_FRAME_INTERVAL / 2).await;
        let too_early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(
            too_early.is_err(),
            "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
        );

        time::advance(MIN_FRAME_INTERVAL).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for clamped draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_rate_limit_does_not_delay_future_draws() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        time::advance(Duration::from_millis(1)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for first draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        requester.schedule_frame_in(Duration::from_millis(50));

        time::advance(Duration::from_millis(49)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
        assert!(early.is_err(), "draw fired too early");

        time::advance(Duration::from_millis(1)).await;
        let second = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for delayed draw");
        assert!(second.is_ok(), "broadcast closed unexpectedly");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_multiple_delayed_requests_coalesce_to_earliest() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame_in(Duration::from_millis(100));
        requester.schedule_frame_in(Duration::from_millis(20));
        requester.schedule_frame_in(Duration::from_millis(120));

        time::advance(Duration::from_millis(10)).await;
        let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
        assert!(early.is_err(), "draw fired too early");

        time::advance(Duration::from_millis(20)).await;
        let first = draw_rx
            .recv()
            .timeout(Duration::from_millis(50))
            .await
            .expect("timed out waiting for earliest coalesced draw");
        assert!(first.is_ok(), "broadcast closed unexpectedly");

        let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
        assert!(second.is_err(), "unexpected extra draw received");
    }
}
