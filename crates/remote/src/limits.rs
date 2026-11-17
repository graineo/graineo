//! Rate limits: requests per address, failed sign-ins per address, renders at a time.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Token bucket per address.
pub struct Buckets {
    rate: f64,
    burst: f64,
    map: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl Buckets {
    pub fn new(rate_per_s: f64, burst: f64) -> Self {
        Buckets { rate: rate_per_s, burst, map: Mutex::new(HashMap::new()) }
    }

    /// Takes `cost` tokens; false when the address is over its limit.
    pub fn take(&self, ip: IpAddr, cost: f64) -> bool {
        let now = Instant::now();
        let mut m = self.map.lock().unwrap();
        if m.len() > 4096 {
            // forget idle addresses (bounded memory)
            m.retain(|_, (_, t)| now.duration_since(*t) < Duration::from_secs(60));
        }
        let e = m.entry(ip).or_insert((self.burst, now));
        let refill = now.duration_since(e.1).as_secs_f64() * self.rate;
        e.0 = (e.0 + refill).min(self.burst);
        e.1 = now;
        if e.0 >= cost {
            e.0 -= cost;
            true
        } else {
            false
        }
    }
}

/// Failed sign-ins: after `max` failures within `window` the address is blocked for `block`.
pub struct Failures {
    max: u32,
    window: Duration,
    block: Duration,
    map: Mutex<HashMap<IpAddr, (u32, Instant, Option<Instant>)>>,
}

impl Failures {
    pub fn new(max: u32, window: Duration, block: Duration) -> Self {
        Failures { max, window, block, map: Mutex::new(HashMap::new()) }
    }

    pub fn blocked(&self, ip: IpAddr) -> bool {
        let m = self.map.lock().unwrap();
        matches!(m.get(&ip), Some((_, _, Some(until))) if Instant::now() < *until)
    }

    pub fn fail(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut m = self.map.lock().unwrap();
        if m.len() > 4096 {
            m.retain(|_, (_, t, b)| now.duration_since(*t) < self.window || b.is_some_and(|u| now < u));
        }
        let e = m.entry(ip).or_insert((0, now, None));
        if now.duration_since(e.1) > self.window {
            *e = (0, now, None);
        }
        e.0 += 1;
        if e.0 >= self.max {
            e.2 = Some(now + self.block);
        }
    }

    pub fn success(&self, ip: IpAddr) {
        self.map.lock().unwrap().remove(&ip);
    }
}

/// Counts running jobs; `try_enter` fails when `max` are running.
pub struct Slots {
    max: usize,
    used: Mutex<usize>,
}

pub struct Slot<'a>(&'a Slots);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.used.lock().unwrap() -= 1;
    }
}

impl Slots {
    pub fn new(max: usize) -> Self {
        Slots { max, used: Mutex::new(0) }
    }

    pub fn used(&self) -> usize {
        *self.used.lock().unwrap()
    }

    pub fn try_enter(&self) -> Option<Slot<'_>> {
        let mut u = self.used.lock().unwrap();
        if *u >= self.max {
            return None;
        }
        *u += 1;
        Some(Slot(self))
    }
}

/// Counts running jobs per address; `try_enter` fails when the address has `max` running.
pub struct PerKey {
    max: usize,
    used: Mutex<HashMap<IpAddr, usize>>,
}

pub struct KeySlot<'a>(&'a PerKey, IpAddr);

impl Drop for KeySlot<'_> {
    fn drop(&mut self) {
        let mut m = self.0.used.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.1) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.1);
            }
        }
    }
}

impl PerKey {
    pub fn new(max: usize) -> Self {
        PerKey { max, used: Mutex::new(HashMap::new()) }
    }

    pub fn try_enter(&self, ip: IpAddr) -> Option<KeySlot<'_>> {
        let mut m = self.used.lock().unwrap_or_else(|e| e.into_inner());
        let n = m.entry(ip).or_insert(0);
        if *n >= self.max {
            return None;
        }
        *n += 1;
        Some(KeySlot(self, ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_limits_bursts() {
        let b = Buckets::new(1.0, 3.0);
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        assert!(b.take(ip, 1.0) && b.take(ip, 1.0) && b.take(ip, 1.0));
        assert!(!b.take(ip, 1.0));
        assert!(b.take("192.168.1.10".parse().unwrap(), 1.0), "other address unaffected");
    }

    #[test]
    fn failures_block() {
        let f = Failures::new(3, Duration::from_secs(60), Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        f.fail(ip);
        f.fail(ip);
        assert!(!f.blocked(ip));
        f.fail(ip);
        assert!(f.blocked(ip));
    }

    #[test]
    fn slots() {
        let s = Slots::new(1);
        let a = s.try_enter();
        assert!(a.is_some() && s.try_enter().is_none());
        drop(a);
        assert!(s.try_enter().is_some());
    }

    #[test]
    fn per_key() {
        let p = PerKey::new(1);
        let (a, b): (IpAddr, IpAddr) = ("10.0.0.2".parse().unwrap(), "10.0.0.3".parse().unwrap());
        let x = p.try_enter(a);
        assert!(x.is_some() && p.try_enter(a).is_none());
        assert!(p.try_enter(b).is_some(), "other address unaffected");
        drop(x);
        assert!(p.try_enter(a).is_some());
        assert!(p.used.lock().unwrap().is_empty(), "no entries left behind");
    }
}
