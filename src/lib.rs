use serde::{Deserialize, Serialize};

use enginelib::{events::ID, prelude::*};
#[derive(Debug, Clone, Default, Serialize, Deserialize, Verifiable)]
pub struct FibTask {
    pub iter: u64,
    pub result: u64,
}

impl Task for FibTask {
    fn to_toml(&self) -> String {
        toml::to_string(&self.clone()).unwrap()
    }
    fn from_toml(&self, d: String) -> Box<dyn Task> {
        let r: Self = toml::from_str(&d).unwrap();
        Box::new(r)
    }
    fn get_id(&self) -> Identifier {
        ID("engine_mod", "fib")
    }
    fn clone_box(&self) -> Box<dyn Task> {
        Box::new(self.clone())
    }
    fn run_cpu(&mut self) {
        let mut a = 0;
        let mut b = 1;
        for _ in 0..self.iter {
            let tmp = a;
            a = b;
            b += tmp;
        }
        self.result = a;
    }
    fn from_bytes(&self, bytes: &[u8]) -> Box<dyn Task> {
        let task: FibTask = enginelib::api::from_bytes(bytes).unwrap();
        Box::new(task)
    }
    fn to_bytes(&self) -> Vec<u8> {
        enginelib::api::to_allocvec(self).unwrap()
    }
}

#[metadata]
pub fn metadata() -> LibraryMetadata {
    LibraryMetadata {
        mod_id: "engine_mod".to_owned(),
        mod_author: "@ign-styly".to_string(),
        mod_name: "Engine mod Demo".to_string(),
        mod_version: "0.0.1".to_string(),
        ..Default::default()
    }
}

#[derive(Clone, Debug, Event)]
#[event(namespace = "engine_mod", name = "custom_event", cancellable)]
pub struct CustomEvent {
    pub cancelled: bool,
    pub message: String,
}

#[event_handler(namespace = "engine_mod", name = "custom_event")]
fn on_custom(event: &mut CustomEvent) {
    info!("custom_event: {}", event.message);
}

#[event_handler(namespace = "core", name = "cgrpc_event")]
fn on_cgrpc(event: &mut CgrpcEvent) {
    if event.handler_id == ID("engine_mod", "grpc") {
        event
            .output
            .write()
            .unwrap()
            .extend_from_slice(event.payload.as_slice());
        info!("handled cgrpc_event for engine_mod.grpc");
    }
}

#[event_handler(
    namespace = "core",
    name = "start_event",
    ctx = std::sync::Arc::new(metadata())
)]
fn on_start(event: &mut StartEvent, meta: &std::sync::Arc<LibraryMetadata>) {
    for module in event.modules.iter() {
        info!("Module loaded: {} ({})", module.mod_id, module.mod_name);
    }
    info!(
        "StartEvent handled by {} by {}",
        meta.mod_name, meta.mod_author
    );
}

#[module]
pub fn run(api: &mut EngineAPI) {
    api.task_registry.register(
        std::sync::Arc::new(FibTask::default()),
        ID("engine_mod", "fib"),
    );
    info!("Hello world from mod!");
    let mut ev = CustomEvent {
        cancelled: false,
        message: "hello from engine_mod".to_string(),
    };
    api.event_bus.fire(&mut ev);
}
