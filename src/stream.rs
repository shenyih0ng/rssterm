use std::{io, pin::Pin, task::Poll, time::Duration};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent};
use tokio_stream::Stream;

pub(crate) struct RateLimitedEventStream {
    _inner: Pin<Box<EventStream>>,
    _timer: Option<Pin<Box<tokio::time::Sleep>>>,

    delay: Duration, // Duration to wait before allowing rate-limited events to be emitted
    pending_event: Option<io::Result<Event>>,
    can_emit: bool,
}

impl RateLimitedEventStream {
    // TODO: allow users to specify event specific delays + generic event filter instead of hardcoding
    pub fn new(delay: Duration) -> Self {
        RateLimitedEventStream {
            _inner: Box::pin(EventStream::default()),
            _timer: None,
            delay,
            pending_event: None,
            can_emit: true,
        }
    }

    fn start_timer(&mut self) {
        self._timer = Some(Box::pin(tokio::time::sleep(self.delay)));
    }

    fn remove_timer(&mut self) {
        self._timer = None;
    }

    fn should_rate_limit(&self, event: &<EventStream as Stream>::Item) -> bool {
        match event {
            // NOTE: mouse scroll events are interpreted as KeyCode::Up and KeyCode::Down
            Ok(Event::Key(KeyEvent {
                code: KeyCode::Up | KeyCode::Down,
                ..
            })) => true,
            _ => false,
        }
    }
}

// Behavior is similar to a leading + trailing debouncer
impl Stream for RateLimitedEventStream {
    type Item = io::Result<Event>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(ref mut timer) = self._timer {
            if timer.as_mut().poll(cx).is_ready() {
                // Timer has completed, reset it and allow emitting events again
                self.remove_timer();
                self.can_emit = true;
                if let Some(event) = self.pending_event.take() {
                    self.can_emit = false;
                    self._timer = Some(Box::pin(tokio::time::sleep(self.delay)));
                    return Poll::Ready(Some(event));
                }
            }
        }

        loop {
            match self._inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(event)) => {
                    match self.should_rate_limit(&event) {
                        // Event matches the filter, handle rate limiting
                        true => {
                            if self.can_emit {
                                self.can_emit = false;
                                self.start_timer();
                                return Poll::Ready(Some(event));
                            } else {
                                // Only store most recent event and discard/ignore others that came during the delay
                                self.pending_event = Some(event);
                                // Continue polling/draining the inner stream to not accumulate backpressure
                            }
                        }
                        // Non-rate-limited events pass through immediately
                        false => return Poll::Ready(Some(event)),
                    }
                }
                Poll::Ready(None) => {
                    if let Some(event) = self.pending_event.take() {
                        return Poll::Ready(Some(event));
                    } else {
                        return Poll::Ready(None);
                    }
                }
                Poll::Pending => break,
            }
        }
        Poll::Pending
    }
}
