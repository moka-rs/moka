#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    hit_count: u64,
    miss_count: u64,
    load_success_count: u64,
    load_failure_count: u64,
    total_load_time: u64,
    eviction_count: u64,
    eviction_weight: u64,
}

impl CacheStats {
    pub fn set_req_counts(&mut self, hit_count: u64, miss_count: u64) -> &mut Self {
        self.hit_count = hit_count;
        self.miss_count = miss_count;
        self
    }

    pub fn set_load_counts(
        &mut self,
        load_success_count: u64,
        load_failure_count: u64,
        total_load_time: u64,
    ) -> &mut Self {
        self.load_success_count = load_success_count;
        self.load_failure_count = load_failure_count;
        self.total_load_time = total_load_time;
        self
    }

    pub fn set_eviction_count(&mut self, eviction_count: u64, eviction_weight: u64) -> &mut Self {
        self.eviction_count = eviction_count;
        self.eviction_weight = eviction_weight;
        self
    }

    pub fn request_count(&self) -> u64 {
        self.hit_count.saturating_add(self.miss_count)
    }

    pub fn hit_count(&self) -> u64 {
        self.hit_count
    }

    pub fn hit_rate(&self) -> f64 {
        let req_count = self.request_count();
        if req_count == 0 {
            1.0
        } else {
            self.hit_count as f64 / req_count as f64
        }
    }

    pub fn miss_count(&self) -> u64 {
        self.miss_count
    }

    pub fn miss_rate(&self) -> f64 {
        let req_count = self.request_count();
        if req_count == 0 {
            0.0
        } else {
            self.miss_count as f64 / req_count as f64
        }
    }

    pub fn load_count(&self) -> u64 {
        self.load_success_count
            .saturating_add(self.load_failure_count)
    }

    pub fn load_success_count(&self) -> u64 {
        self.load_success_count
    }

    pub fn load_failure_count(&self) -> u64 {
        self.load_failure_count
    }

    pub fn load_failure_rate(&self) -> f64 {
        let load_count = self.load_count();
        if load_count == 0 {
            0.0
        } else {
            self.load_failure_count as f64 / load_count as f64
        }
    }

    pub fn total_load_time(&self) -> u64 {
        self.total_load_time
    }

    pub fn average_load_penalty(&self) -> f64 {
        let load_count = self.load_count();
        if load_count == 0 {
            0.0
        } else {
            self.total_load_time as f64 / load_count as f64
        }
    }

    pub fn eviction_count(&self) -> u64 {
        self.eviction_count
    }

    pub fn eviction_weight(&self) -> u64 {
        self.eviction_weight
    }
}
