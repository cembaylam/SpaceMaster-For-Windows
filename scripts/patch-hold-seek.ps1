$f = "src/viewer.rs"
$t = [IO.File]::ReadAllText($f)
function Rep($old, $new) {
    if (-not $script:t.Contains($old)) { throw "missing: $old" }
    $script:t = $script:t.Replace($old, $new)
}

Rep '    seek_hold: Option<(u16, f64, Instant)>,' '    seek_hold: Option<Hold>,'

Rep 'pub struct Viewer {' @'
/// Left/Right held down on a video: continuous scrubbing (see `Viewer::tick`).
struct Hold {
    vk: u16,
    /// Signed step of the key (10 s, Shift 30 s, Ctrl 1 min ...).
    secs: f64,
    started: Instant,
    last: Instant,
    applied: Instant,
}

pub struct Viewer {
'@

Rep @'
                dirty = self.ticks % 4 == 0 || self.scrubbing || self.seek_hold.is_some() || self.video_pending;
            }
            // our own key repeat while Left/Right is held (110 ms, no OS repeat delay)
            if let Some((vk, secs, last)) = self.seek_hold {
                if !key_is_down(vk as i32) {
                    self.seek_hold = None;
                } else if last.elapsed() >= Duration::from_millis(110) {
                    self.seek_hold = Some((vk, secs, Instant::now()));
                    self.seek_rel(secs);
                }
            }
'@ @'
                dirty = (self.seek_hold.is_none() && self.ticks % 4 == 0)
                    || (self.seek_hold.is_some() && self.ticks % 2 == 0)
                    || self.scrubbing
                    || self.video_pending;
            }
            self.scrub_while_held();
'@

Rep @'
        let secs = dir * Self::step_seconds(&ev);
        self.seek_rel(secs);
        self.seek_hold = Some((vk, secs, Instant::now()));
        Act::None
    }
'@ @'
        let secs = dir * Self::step_seconds(&ev);
        self.seek_rel(secs);
        let now = Instant::now();
        self.seek_hold = Some(Hold { vk, secs, started: now, last: now, applied: now });
        Act::None
    }

    /// True while a seek key is held: the window should tick fast (about 60 Hz) for smooth scrubbing.
    pub fn fast_timer(&self) -> bool {
        self.seek_hold.is_some()
    }

    /// Continuous scrubbing while Left/Right is held. The first press already jumped one step; after a short delay
    /// the position moves smoothly at 6 steps per second (60 s/s for the 10 s key) and accelerates to 8x within
    /// about 1.5 s. The target moves every tick, the player is asked to seek at most every 50 ms (fast keyframe
    /// seeks), and the exact final position is sought when the key is released.
    fn scrub_while_held(&mut self) {
        let Some(h) = &mut self.seek_hold else { return };
        let now = Instant::now();
        let Some(p) = &self.player else { return };
        let len = p.length_ms();
        if !key_is_down(h.vk as i32) {
            p.seek_to_ms(self.seek_target);
            self.last_seek_applied = Some(now);
            self.seek_hold = None;
            return;
        }
        let dt = now.duration_since(h.last).as_secs_f64().min(0.1);
        h.last = now;
        let held = now.duration_since(h.started).as_secs_f64();
        if held < 0.2 || len <= 0 {
            return;
        }
        let ramp = ((held - 0.2) / 1.3).clamp(0.0, 1.0);
        let speed = h.secs * 6.0 * (1.0 + 7.0 * ramp); // seconds of video per second
        self.seek_target = (self.seek_target + (speed * dt * 1000.0) as i64).clamp(0, (len - 300).max(0));
        if now.duration_since(h.applied) >= Duration::from_millis(50) {
            p.seek_to_ms(self.seek_target);
            h.applied = now;
        }
        self.last_seek_applied = Some(now);
        self.last_seek_tick = Some(now);
        self.bar_until = now + Duration::from_millis(1100);
    }
'@

[IO.File]::WriteAllText($f, $t)
"patched"
