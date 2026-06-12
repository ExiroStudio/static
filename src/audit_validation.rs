#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn test_p2_graceful_termination() {
        use crate::runner::NativeProcess;
        
        // Spawn a process that ignores SIGTERM (trap "" 15)
        // We use a script that sleeps then exits.
        let mut p = NativeProcess::spawn("sh", &["-c".to_string(), "trap \"\" 15; sleep 2".to_string()]).unwrap();
        assert!(p.is_alive());
        
        let start = std::time::Instant::now();
        p.terminate();
        let elapsed = start.elapsed();
        
        // Should take at least 50ms due to the grace period in terminate()
        assert!(elapsed >= Duration::from_millis(50), "Termination took only {:?}, expected >= 50ms", elapsed);
        assert!(!p.is_alive());
    }

    // P0/P1 tests usually require WGPU or more setup than available in headless CI.
    // They are verified by Engine::rebuild logic which we've audited.
}
