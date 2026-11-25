//! 公共测试注册（integrator 独占；crate tspi-rt-host）。
//!
//! 约定：leaf 测试放 tests/<Txx>/ 子目录，合入时由 integrator 在此加 #[path] mod 行。
//! T06 scaffold 阶段：尚无 leaf 测试注册（T06 自证测试在独立的顶层 tests/*.rs）。

/// WBS T90：Linux 真实 ALSA 后端依赖必须 target-specific，Windows/macOS 开发与 CI 不编译。
#[test]
fn alsa_backend_dep_is_target_gated() {
    #[cfg(target_os = "linux")]
    assert!(tspi_rt_host::REAL_ALSA_BACKEND_DEP);
    #[cfg(not(target_os = "linux"))]
    assert!(!tspi_rt_host::REAL_ALSA_BACKEND_DEP);
}
