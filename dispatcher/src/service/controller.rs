use std::os::fd::RawFd;

use tokio_util::sync::CancellationToken;

use crate::service::process::{ControlMessage, Process};

pub struct Controller {
    running_services: Vec<Process>,
    max_num_services: usize,
    max_invocation_capacity: usize,
    max_loaded_modules: usize,
    runner_bin: String,
    cancel_token: CancellationToken,
}

impl Controller {
    pub fn new(
        max_num_services: usize,
        max_invocation_capacity: usize,
        max_loaded_modules: usize,
        runner_bin: String,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            running_services: Vec::new(),
            max_num_services,
            max_invocation_capacity,
            max_loaded_modules,
            runner_bin,
            cancel_token,
        }
    }

    /// Schedule an invocation on a process. Prefers a process that already has
    /// the module loaded, falls back to the first process with capacity, and
    /// spawns a new process as a last resort.
    pub async fn schedule_invocation(
        &mut self,
        service_id: String,
        wasm_bytes: Vec<u8>,
        tcp_fd: RawFd,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1. Try to find a process that already has the module loaded and has capacity.
        let warm = self.running_services.iter().position(|p| {
            p.has_runtime_loaded(&service_id) && p.has_capacity()
        });

        if let Some(idx) = warm {
            let process = &mut self.running_services[idx];
            eprintln!(
                "[controller] routing to warm process pid={} (running: {}/{})",
                process.pid, process.running_invocations(), process.max_invocation_capacity
            );
            process.increment_invocations();
            process.track_module(service_id.clone());
            process.sender.send(ControlMessage::Invocation { service_id, wasm_bytes, tcp_fd }).await?;
            return Ok(());
        }

        // 2. Fall back to the first process with available capacity.
        let available = self.running_services.iter().position(|p| p.has_capacity());

        if let Some(idx) = available {
            let process = &mut self.running_services[idx];
            eprintln!(
                "[controller] routing to available process pid={} (running: {}/{})",
                process.pid, process.running_invocations(), process.max_invocation_capacity
            );
            process.increment_invocations();
            process.track_module(service_id.clone());
            process.sender.send(ControlMessage::Invocation { service_id, wasm_bytes, tcp_fd }).await?;
            return Ok(());
        }

        // 3. No capacity anywhere — spawn a new process if allowed.
        if self.running_services.len() == self.max_num_services {
            return Err("can not create any more processes".into());
        }

        eprintln!(
            "[controller] no available process, spawning new ({}/{})",
            self.running_services.len() + 1,
            self.max_num_services
        );

        let mut process = Process::run(
            &self.runner_bin,
            self.max_invocation_capacity,
            self.max_loaded_modules,
            self.cancel_token.clone(),
        )?;
        process.increment_invocations();
        process.track_module(service_id.clone());
        process.sender.send(ControlMessage::Invocation { service_id, wasm_bytes, tcp_fd }).await?;
        self.running_services.push(process);

        Ok(())
    }
}
