//! 双轨 3 点滑动中值滤波（对应 daemon.sh median3 / push_dual_samples）。

/// 三值排序取中（对应 median3）。
pub fn median3(a: i32, b: i32, c: i32) -> i32 {
    let (mut a, mut b, mut c) = (a, b, c);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    if b > c {
        std::mem::swap(&mut b, &mut c);
    }
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    b
}

/// 单条 3 点滑动中值窗（前 2 点用均值，之后滑动取中值）。
#[derive(Clone, Debug)]
pub struct FilterWindow {
    cnt: i32,
    w1: i32,
    w2: i32,
    w3: i32,
    pub flt: i32,
}

impl FilterWindow {
    pub fn new() -> Self {
        FilterWindow {
            cnt: 0,
            w1: 0,
            w2: 0,
            w3: 0,
            flt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.cnt = 0;
        self.w1 = 0;
        self.w2 = 0;
        self.w3 = 0;
        self.flt = 0;
    }

    pub fn push(&mut self, v: i32) -> i32 {
        match self.cnt {
            0 => {
                self.w1 = v;
                self.cnt = 1;
                self.flt = v;
            }
            1 => {
                self.w2 = v;
                self.cnt = 2;
                self.flt = (self.w1 + self.w2 + 1) / 2;
            }
            2 => {
                self.w3 = v;
                self.cnt = 3;
                self.flt = median3(self.w1, self.w2, self.w3);
            }
            _ => {
                self.w1 = self.w2;
                self.w2 = self.w3;
                self.w3 = v;
                self.flt = median3(self.w1, self.w2, self.w3);
            }
        }
        self.flt
    }
}
