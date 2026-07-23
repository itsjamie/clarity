use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct RateLimitService {
    entries: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    window: Duration,
}

impl RateLimitService {
    #[must_use]
    pub fn per_minute() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            window: Duration::from_secs(60),
        }
    }

    pub fn check(&self, scope: &str, key: &str, limit: u32) -> bool {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let bucket = entries.entry(format!("{scope}:{key}")).or_default();
        while bucket.front().is_some_and(|timestamp| *timestamp <= cutoff) {
            bucket.pop_front();
        }
        if bucket.len() >= limit as usize {
            return false;
        }
        bucket.push_back(now);
        if entries.len() > 10_000 {
            entries.retain(|_, values| values.back().is_some_and(|timestamp| *timestamp > cutoff));
        }
        true
    }
}

#[derive(Debug)]
pub struct SessionRateLimiter {
    events: VecDeque<Instant>,
    limit: u32,
    window: Duration,
}

impl SessionRateLimiter {
    #[must_use]
    pub fn per_minute(limit: u32) -> Self {
        Self {
            events: VecDeque::new(),
            limit,
            window: Duration::from_secs(60),
        }
    }

    pub fn check(&mut self) -> bool {
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while self
            .events
            .front()
            .is_some_and(|timestamp| *timestamp <= cutoff)
        {
            self.events.pop_front();
        }
        if self.events.len() >= self.limit as usize {
            return false;
        }
        self.events.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_requests_after_the_limit() {
        let limiter = RateLimitService::per_minute();
        assert!(limiter.check("room", "source", 2));
        assert!(limiter.check("room", "source", 2));
        assert!(!limiter.check("room", "source", 2));
        assert!(limiter.check("room", "other", 2));
    }
}
