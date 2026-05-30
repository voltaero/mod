use enginelib::{events::ID, prelude::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default, Serialize, Deserialize, Verifiable)]
pub struct PowTask {
    pub seed: u32,
    pub zeros: u8,
    pub nonce: u64,
}
//lgtm
impl PowTask {
    #[inline]
    fn has_leading_zero_nibbles(digest: &[u8; 32], zeros: u8) -> bool {
        let zeros = usize::from(zeros).min(64);
        let full_zero_bytes = zeros / 2;

        if digest[..full_zero_bytes].iter().any(|&b| b != 0) {
            return false;
        }

        if zeros % 2 == 1 {
            (digest[full_zero_bytes] & 0xF0) == 0
        } else {
            true
        }
    }
}

impl Task for PowTask {
    fn to_toml(&self) -> String {
        toml::to_string(&self.clone()).unwrap()
    }
    fn from_toml(&self, d: String) -> Box<dyn Task> {
        let r: Self = toml::from_str(&d).unwrap();
        Box::new(r)
    }
    fn get_id(&self) -> Identifier {
        ID("engine_mod", "pow")
    }
    fn clone_box(&self) -> Box<dyn Task> {
        Box::new(self.clone())
    }
    fn run_cpu(&mut self) {
        let zeros = self.zeros.min(64);
        let seed_bytes = self.seed.to_le_bytes();

        for n in 0..=u64::MAX {
            let mut hasher = Sha256::new();
            hasher.update(n.to_le_bytes());
            hasher.update(seed_bytes);
            let digest: [u8; 32] = hasher.finalize().into();

            if Self::has_leading_zero_nibbles(&digest, zeros) {
                self.nonce = n;
                return;
            }
        }

        warn!(
            "unable to find nonce for seed='{}' and zeros={}",
            self.seed, self.zeros
        );
    }
    fn from_bytes(&self, bytes: &[u8]) -> Box<dyn Task> {
        let task: PowTask = enginelib::api::from_bytes(bytes).unwrap();
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
        std::sync::Arc::new(PowTask::default()),
        ID("engine_mod", "pow"),
    );
    info!("Hello world from mod!");
    let mut ev = CustomEvent {
        cancelled: false,
        message: "hello from engine_mod".to_string(),
    };
    api.event_bus.fire(&mut ev);
}
