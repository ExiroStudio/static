use std::collections::HashMap;

pub struct OutputScheduler {
    target_fps: HashMap<String, f32>,
    last_frame_time: HashMap<String, f64>,
}

impl OutputScheduler {
    pub fn new() -> Self {
        Self {
            target_fps: HashMap::new(),
            last_frame_time: HashMap::new(),
        }
    }

    pub fn register_sink(&mut self, id: &str, fps: f32) {
        self.target_fps.insert(id.to_string(), fps);
        self.last_frame_time.insert(id.to_string(), 0.0);
    }

    pub fn unregister_sink(&mut self, id: &str) {
        self.target_fps.remove(id);
        self.last_frame_time.remove(id);
    }

    /// Determines if a sink should receive a frame based on the current engine time.
    /// Returns true if the sink should render this frame.
    pub fn should_render(&mut self, id: &str, current_time: f64) -> bool {
        if let Some(&fps) = self.target_fps.get(id) {
            if fps <= 0.0 {
                return false;
            }
            let interval = 1.0 / (fps as f64);
            if let Some(&last_time) = self.last_frame_time.get(id) {
                if current_time - last_time >= interval {
                    self.last_frame_time.insert(id.to_string(), current_time);
                    return true;
                }
                return false;
            } else {
                self.last_frame_time.insert(id.to_string(), current_time);
                return true;
            }
        }
        // If not registered with a specific FPS, render every frame by default
        true
    }
}
