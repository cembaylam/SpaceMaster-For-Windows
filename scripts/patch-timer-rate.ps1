$f = "src/app.rs"
$t = [IO.File]::ReadAllText($f)
function Rep($old, $new) {
    if (-not $script:t.Contains($old)) { throw "missing: $old" }
    $script:t = $script:t.Replace($old, $new)
}

Rep '    timer_on: bool,' "    timer_on: bool,`n    timer_fast: bool,"
Rep '            timer_on: false,' "            timer_on: false,`n            timer_fast: false,"

Rep @'
        if self.viewer.active {
            if self.viewer.tick() {
                self.invalidate();
            }
            if !self.viewer.needs_timer() {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ID);
                }
                self.timer_on = false;
            }
            return;
        }
'@ @'
        if self.viewer.active {
            if self.viewer.tick() {
                self.invalidate();
            }
            self.sync_timer_rate();
            if !self.viewer.needs_timer() {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ID);
                }
                self.timer_on = false;
                self.timer_fast = false;
            }
            return;
        }
'@

Rep '    fn timer(&mut self) {' @'
    /// ~60 Hz while a seek key is held on a video (smooth scrubbing), the normal 12 Hz otherwise.
    fn sync_timer_rate(&mut self) {
        let fast = self.viewer.active && self.viewer.fast_timer();
        if fast != self.timer_fast && self.timer_on {
            self.timer_fast = fast;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ID, if fast { 16 } else { 1000 / 12 }, None);
            }
        }
    }

    fn timer(&mut self) {
'@

# a key press starts the hold: speed the timer up right away
Rep @'
        self.invalidate();
        self.ensure_timer();
    }

    /// n / p: the next / previous file
'@ @'
        self.invalidate();
        self.ensure_timer();
        self.sync_timer_rate();
    }

    /// n / p: the next / previous file
'@

[IO.File]::WriteAllText($f, $t)
"patched"
