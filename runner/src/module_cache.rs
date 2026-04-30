use std::collections::{HashMap, VecDeque};

use wasmtime::Engine;
use wasmtime::component::Component;

/// A bounded FIFO cache of compiled WASM components keyed by service ID.
///
/// Mirrors the dispatcher's `loaded_modules` queue: when the cache is full,
/// the oldest entry is evicted to make room for the new one. Components that
/// are already cached get moved to the back (most recently used).
pub struct ModuleCache {
    engine: Engine,
    order: VecDeque<String>,
    components: HashMap<String, Component>,
    capacity: usize,
}

impl ModuleCache {
    pub fn new(engine: Engine, capacity: usize) -> Self {
        Self {
            engine,
            order: VecDeque::with_capacity(capacity),
            components: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// get a compiled component from the cache, or compile and insert it.
    pub fn get_or_compile(
        &mut self,
        service_id: &str,
        wasm_bytes: &[u8],
    ) -> wasmtime::Result<Component> {
        if let Some(component) = self.components.get(service_id) {
            // Move to back of FIFO (most recently used).
            if let Some(pos) = self.order.iter().position(|id| id == service_id) {
                self.order.remove(pos);
            }
            self.order.push_back(service_id.to_string());
            return Ok(component.clone());
        }

        // compile component bytes
        let component = Component::new(&self.engine, wasm_bytes)?;

        // Evict oldest if at capacity.
        if self.order.len() == self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.components.remove(&evicted);
            }
        }

        self.order.push_back(service_id.to_string());
        self.components.insert(service_id.to_string(), component.clone());

        Ok(component)
    }
}
