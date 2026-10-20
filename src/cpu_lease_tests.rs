//! Fault injection at manager I/O; calls the production lease lifecycle and real
//! durable files. No live processes, environment changes or user units involved.
use super::*;
use std::cell::RefCell;

const UNIT: &str = "dracon-protection-test-fixture.scope";
const BOOT: &str = "fixture-boot";

struct Model {
    quota: u64,
    invocation: String,
    unavailable: bool,
    load_state: String,
    reject_arm: bool,
    fail_write: bool,
    ambiguous_write: bool,
    replace_on_arm: bool,
    timer_active: bool,
    writes: Vec<(String, u64)>,
    stopped: Vec<String>,
    commands: Vec<Vec<String>>,
}
struct FakeManager {
    root: tempfile::TempDir,
    state: RefCell<Model>,
}
impl FakeManager {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            state: RefCell::new(Model {
                quota: u64::MAX,
                invocation: "first".into(),
                unavailable: false,
                load_state: "loaded".into(),
                reject_arm: false,
                fail_write: false,
                ambiguous_write: false,
                replace_on_arm: false,
                timer_active: false,
                writes: vec![],
                stopped: vec![],
                commands: vec![],
            }),
        }
    }
    async fn cap(&self) -> Result<(String, String), String> {
        cap_prepared(self, self.root.path(), BOOT, UNIT, 50, 3600, || Ok(())).await
    }
    async fn expire(&self, key: &str, generation: &str) -> Result<(), String> {
        // This is the same entry point used by release() in the expiry helper.
        release_with(self, self.root.path(), BOOT, key, generation).await
    }
    fn saved(&self) -> Option<Lease> {
        load(self.root.path(), &lease_key(UNIT)).unwrap()
    }
    fn safe(&self) {
        let state = self.state.borrow();
        assert!(state.stopped.iter().all(|u| u.ends_with(".timer")));
        for command in &state.commands {
            assert!(command.iter().any(|s| s == "cpu-lease-release"));
            assert!(!command
                .iter()
                .any(|s| s == UNIT || s.contains("RuntimeMaxSec") || s.contains("cgroup.procs")));
            assert!(command.iter().any(|s| s == "--property=Restart=on-failure"));
            assert!(command
                .iter()
                .any(|s| s == "--property=StartLimitIntervalSec=0"));
        }
        assert!(state.writes.iter().all(|(unit, _)| unit == UNIT));
    }
}
impl LeaseManager for FakeManager {
    async fn property(&self, unit: &str, name: &str) -> Result<String, String> {
        let s = self.state.borrow();
        if s.unavailable {
            return Err("manager unavailable".into());
        }
        match name {
            "InvocationID" => Ok(s.invocation.clone()),
            "LoadState" => Ok(s.load_state.clone()),
            "Delegate" => Ok("no".into()),
            "ActiveState" if unit.ends_with(".timer") => {
                Ok(if s.timer_active { "active" } else { "inactive" }.into())
            }
            "ActiveState" => Ok("active".into()),
            "CPUQuotaPerSecUSec" => Ok(s.quota.to_string()),
            _ => panic!("unexpected manager query: {unit} {name}"),
        }
    }
    async fn set_quota(&self, unit: &str, quota: u64) -> Result<(), String> {
        // Even ambiguous failures must have a persisted, armed intent first.
        let lease = self.saved().expect("write-ahead intent missing");
        assert!(lease.armed);
        let mut s = self.state.borrow_mut();
        s.writes.push((unit.into(), quota));
        if s.unavailable {
            return Err("manager unavailable".into());
        }
        if s.fail_write {
            if s.ambiguous_write {
                s.quota = quota;
            }
            return Err("SetUnitProperties rejected or response lost".into());
        }
        s.quota = quota;
        Ok(())
    }
    async fn arm(
        &self,
        timer: &str,
        seconds: u64,
        key: &str,
        generation: &str,
    ) -> Result<(), String> {
        let saved = self.saved().expect("intent must precede timer creation");
        assert!(!saved.armed);
        assert_eq!(saved.generation, generation);
        let mut s = self.state.borrow_mut();
        s.commands.push(expiry_args(
            timer,
            seconds,
            Path::new("/fixture/dracon-system"),
            key,
            generation,
        ));
        if s.reject_arm {
            return Err("StartTransientUnit denied".into());
        }
        s.timer_active = true;
        if s.replace_on_arm {
            s.invocation = "replacement".into();
        }
        Ok(())
    }
    async fn stop_timer(&self, timer: &str) {
        let mut s = self.state.borrow_mut();
        s.stopped.push(format!("{timer}.timer"));
        s.timer_active = false;
    }
}

#[tokio::test]
async fn rejected_timer_retains_intent_without_applying_and_restart_retires_it() {
    let m = FakeManager::new();
    m.state.borrow_mut().reject_arm = true;
    assert!(m.cap().await.unwrap_err().contains("denied"));
    let old = m.saved().unwrap();
    assert!(!old.armed && !old.applied_verified);
    assert!(m.state.borrow().writes.is_empty());
    m.state.borrow_mut().unavailable = true;
    assert!(m.expire(&lease_key(UNIT), &old.generation).await.is_err());
    assert!(m.saved().is_some());
    m.state.borrow_mut().unavailable = false;
    // Recovery uses only durable state, not a successful cap() return value.
    m.expire(&lease_key(UNIT), &old.generation).await.unwrap();
    assert!(m.saved().is_none());
    assert_eq!(m.state.borrow().quota, u64::MAX);
    assert!(m.state.borrow().writes.is_empty());
    m.safe();
}

#[tokio::test]
async fn rejected_application_retains_debt_and_next_pass_recovers_before_reapplying() {
    let m = FakeManager::new();
    m.state.borrow_mut().fail_write = true;
    assert!(m.cap().await.is_err());
    let lease = m.saved().unwrap();
    assert!(lease.armed && !lease.applied_verified);
    assert_eq!(m.state.borrow().quota, u64::MAX);
    m.state.borrow_mut().fail_write = false;
    assert!(m.cap().await.unwrap_err().contains("recovered"));
    assert!(m.saved().is_none());
    let (key, generation) = m.cap().await.unwrap();
    assert_eq!(m.state.borrow().quota, 500_000);
    m.expire(&key, &generation).await.unwrap();
    assert_eq!(m.state.borrow().quota, u64::MAX);
    assert!(m.saved().is_none());
    m.safe();
}

#[tokio::test]
async fn ambiguous_application_and_failed_expiry_remain_recoverable_until_retry_succeeds() {
    let m = FakeManager::new();
    {
        let mut s = m.state.borrow_mut();
        s.fail_write = true;
        s.ambiguous_write = true;
    }
    assert!(m.cap().await.is_err());
    let lease = m.saved().unwrap();
    assert!(!lease.applied_verified);
    assert_eq!(m.state.borrow().quota, 500_000);
    // The independent expiry invocation fails; the JSON obligation must remain.
    m.state.borrow_mut().ambiguous_write = false;
    assert!(m.expire(&lease_key(UNIT), &lease.generation).await.is_err());
    assert!(m.saved().is_some());
    assert_eq!(m.state.borrow().quota, 500_000);
    assert!(m.state.borrow().stopped.is_empty());
    m.state.borrow_mut().fail_write = false;
    m.expire(&lease_key(UNIT), &lease.generation).await.unwrap();
    assert_eq!(m.state.borrow().quota, u64::MAX);
    assert!(m.saved().is_none());
    m.safe();
}

#[tokio::test]
async fn expiry_manager_outage_preserves_verified_lease_across_reload_then_restores() {
    let m = FakeManager::new();
    let (key, generation) = m.cap().await.unwrap();
    m.state.borrow_mut().unavailable = true;
    assert!(m.expire(&key, &generation).await.is_err());
    assert!(m.saved().unwrap().applied_verified);
    assert_eq!(m.state.borrow().quota, 500_000);
    assert!(m.state.borrow().stopped.is_empty());
    // A startup recovery pass during the outage must retain the same debt.
    recover_with(&m, m.root.path(), BOOT).await;
    assert!(m.saved().is_some());
    m.state.borrow_mut().unavailable = false;
    recover_with(&m, m.root.path(), BOOT).await;
    assert_eq!(m.state.borrow().quota, u64::MAX);
    assert!(m.saved().is_none());
    m.safe();
}

#[tokio::test]
async fn replaced_unit_before_application_and_before_expiry_is_never_mutated() {
    let m = FakeManager::new();
    m.state.borrow_mut().replace_on_arm = true;
    assert!(m.cap().await.unwrap_err().contains("identity changed"));
    let old = m.saved().unwrap();
    assert!(m.state.borrow().writes.is_empty());
    m.expire(&lease_key(UNIT), &old.generation).await.unwrap();
    assert!(m.saved().is_none());
    assert!(m.state.borrow().writes.is_empty());
    m.state.borrow_mut().replace_on_arm = false;
    let (key, generation) = m.cap().await.unwrap();
    {
        let mut s = m.state.borrow_mut();
        s.invocation = "third".into();
        s.quota = 200_000;
    }
    let writes_before = m.state.borrow().writes.len();
    m.expire(&key, &generation).await.unwrap();
    assert_eq!(m.state.borrow().quota, 200_000);
    assert_eq!(m.state.borrow().writes.len(), writes_before);
    assert!(m.saved().is_none());
    m.safe();
}

#[tokio::test]
async fn uncertain_unit_retains_debt_until_confirmed_disappearance_without_writes() {
    let m = FakeManager::new();
    let (key, generation) = m.cap().await.unwrap();
    let writes = m.state.borrow().writes.len();
    m.state.borrow_mut().load_state = "error".into();
    assert!(m.expire(&key, &generation).await.is_err());
    assert!(m.saved().is_some());
    assert!(m.state.borrow().stopped.is_empty());
    m.state.borrow_mut().load_state = "not-found".into();
    m.expire(&key, &generation).await.unwrap();
    assert!(m.saved().is_none());
    assert_eq!(m.state.borrow().writes.len(), writes);
    m.safe();
}

#[tokio::test]
async fn workload_revalidation_failure_leaves_only_unapplied_intent() {
    let m = FakeManager::new();
    let error = cap_prepared(&m, m.root.path(), BOOT, UNIT, 50, 3600, || {
        Err("workload vanished or process incarnation changed".into())
    })
    .await
    .unwrap_err();
    assert!(error.contains("incarnation"));
    assert!(m.saved().is_some());
    assert!(m.state.borrow().writes.is_empty());
    recover_with(&m, m.root.path(), BOOT).await;
    assert!(m.saved().is_none());
    assert!(m.state.borrow().writes.is_empty());
    m.safe();
}

#[tokio::test]
async fn stale_timer_and_external_quota_preserve_current_ownership() {
    let m = FakeManager::new();
    let (key, generation) = m.cap().await.unwrap();
    m.expire(&key, "obsolete-generation").await.unwrap();
    assert!(m.saved().is_some());
    assert_eq!(m.state.borrow().quota, 500_000);
    m.state.borrow_mut().quota = 250_000;
    m.expire(&key, &generation).await.unwrap();
    assert_eq!(m.state.borrow().quota, 250_000);
    assert!(m.saved().is_none());
    m.safe();
}
