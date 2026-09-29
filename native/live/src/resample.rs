//! Rate adaptation for devices that do not run at the source's rate (e.g.
//! Bluetooth headphones at 44.1 kHz while the SHARC renders 48 kHz).
//!
//! [`RateAdapter`] wraps a [`FrameSource`] and linearly interpolates its
//! output to the device rate, rendering source frames as it needs them.
//! Linear interpolation is enough to listen with (it attenuates the top
//! octave slightly and aliases a little); it is not part of any
//! bit-exactness check, which reads the source's own output.

use crate::ring::{FRAME_LEN, StereoSample};
use crate::source::FrameSource;

pub struct RateAdapter {
    inner: Box<dyn FrameSource>,
    /// Source samples per output sample.
    step: f64,
    /// Buffered source samples; `pos` indexes into it.
    buf: Vec<StereoSample>,
    pos: f64,
    frame: [StereoSample; FRAME_LEN],
}

impl RateAdapter {
    pub fn new(inner: Box<dyn FrameSource>, source_rate: u32, device_rate: u32) -> Self {
        RateAdapter {
            inner,
            step: source_rate as f64 / device_rate as f64,
            buf: Vec::with_capacity(8 * FRAME_LEN),
            pos: 0.0,
            frame: [StereoSample::default(); FRAME_LEN],
        }
    }

    fn pull(&mut self, input: &[u8]) {
        self.inner.render_frame(input, &mut self.frame);
        self.buf.extend_from_slice(&self.frame);
    }
}

impl FrameSource for RateAdapter {
    fn render_frame(&mut self, input_frame: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
        for o in out.iter_mut() {
            let i = self.pos as usize;
            while i + 1 >= self.buf.len() {
                self.pull(input_frame);
            }
            let f = (self.pos - i as f64) as f32;
            let (a, b) = (self.buf[i], self.buf[i + 1]);
            *o = StereoSample {
                l: a.l + (b.l - a.l) * f,
                r: a.r + (b.r - a.r) * f,
            };
            self.pos += self.step;
        }
        // Drop consumed samples, keeping the one the next output starts at.
        let keep = self.pos as usize;
        if keep > 0 {
            self.buf.drain(..keep);
            self.pos -= keep as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emits 0, 1, 2, ... on both channels.
    struct Ramp(f32);
    impl FrameSource for Ramp {
        fn render_frame(&mut self, _i: &[u8], out: &mut [StereoSample; FRAME_LEN]) {
            for o in out.iter_mut() {
                *o = StereoSample {
                    l: self.0,
                    r: -self.0,
                };
                self.0 += 1.0;
            }
        }
    }

    #[test]
    fn equal_rates_pass_through() {
        let mut a = RateAdapter::new(Box::new(Ramp(0.0)), 48_000, 48_000);
        let mut out = [StereoSample::default(); FRAME_LEN];
        for k in 0..4 {
            a.render_frame(&[], &mut out);
            for (i, s) in out.iter().enumerate() {
                assert_eq!(s.l, (k * FRAME_LEN + i) as f32);
                assert_eq!(s.r, -s.l);
            }
        }
    }

    #[test]
    fn downsampling_advances_by_the_ratio() {
        let mut a = RateAdapter::new(Box::new(Ramp(0.0)), 48_000, 44_100);
        let mut out = [StereoSample::default(); FRAME_LEN];
        let step = 48_000.0 / 44_100.0;
        for k in 0..100 {
            a.render_frame(&[], &mut out);
            for (i, s) in out.iter().enumerate() {
                let want = ((k * FRAME_LEN + i) as f64 * step) as f32;
                assert!(
                    (s.l - want).abs() < 1e-2 * want.max(1.0),
                    "{k} {i}: {} vs {want}",
                    s.l
                );
            }
        }
        assert!(a.buf.len() < 4 * FRAME_LEN, "buffer stays bounded");
    }
}
