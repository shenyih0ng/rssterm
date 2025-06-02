use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Stylize},
    widgets::Widget,
};
use ratatui_macros::{span, text};

#[derive(Debug)]
pub(crate) struct FpsWidget {
    frame_count: usize,
    last_instant: Instant,

    curr_fps: Option<f32>,
    prev_fps: Option<f32>,
}

impl Default for FpsWidget {
    fn default() -> Self {
        Self {
            frame_count: 0,
            last_instant: Instant::now(),
            curr_fps: None,
            prev_fps: None,
        }
    }
}

impl Widget for &mut FpsWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.frame_count += 1;

        let elapsed = self.last_instant.elapsed();
        if elapsed > Duration::from_secs(1) && self.frame_count > 2 {
            self.prev_fps = self.curr_fps;
            self.curr_fps = Some(self.frame_count as f32 / elapsed.as_secs_f32());

            self.frame_count = 0;
            self.last_instant = Instant::now();
        }

        if let Some(curr_fps) = self.curr_fps {
            let mut fps_text = vec![span!("{:.2} fps", curr_fps).green()];

            if let Some(prev_fps) = self.prev_fps {
                let p_delta = if prev_fps == 0.0 {
                    100.0
                } else {
                    ((curr_fps - prev_fps) / prev_fps * 100.0).abs()
                };
                // If the delta is less than 2%, we consider it no change
                let p_no_change = p_delta < 2.0;

                let p_delta_symbol = if prev_fps < curr_fps { "▲" } else { "▼" };
                let p_delta_span = span!(format!(" {} {:.2}% ", p_delta_symbol, p_delta))
                    .fg(Color::Rgb(255, 255, 255));

                // Padding for readability
                fps_text.push(span!(" "));
                fps_text.push(if p_no_change {
                    p_delta_span
                } else if prev_fps < curr_fps {
                    p_delta_span.bg(Color::Rgb(22, 163, 74))
                } else {
                    p_delta_span.bg(Color::Rgb(220, 38, 38))
                });
            }

            text![fps_text]
                .alignment(ratatui::layout::Alignment::Right)
                .render(area, buf);
        }
    }
}
