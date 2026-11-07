//! 按应用伪装时区：私有属性副本 + Java 默认时区缓存 + libc TZ 缓存。
//!
//! 不写系统属性服务，不安装驻留 hook，处理结束后仍可卸载模块。

use std::collections::{HashMap, HashSet};

use anyhow::{bail, ensure};
use jni::{
    Env, EnvUnowned, errors::LogErrorAndDefault, jni_sig, jni_str,
    objects::{JObject, JString, JValue},
};
use log::info;

use crate::{config::TIMEZONE_PROPERTY, cow_props};

/// 仅接受区域 ID 和 UTC/GMT，不接受 Java 与 POSIX 符号含义相反的 GMT+08:00。
/// 是否存在由设备自身的 Java tzdata 再校验，避免未知 ID 静默回退到 GMT。
fn valid_timezone_id(id: &str) -> bool {
    if matches!(id, "UTC" | "GMT") {
        return true;
    }
    id.len() < 92
        && id.contains('/')
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && id.split('/').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+')
                })
        })
}

/// 时区失败只跳过本功能，不把 JNI 异常带回应用启动流程。
fn try_jni<T: Default>(
    env: &mut EnvUnowned,
    operation: impl FnOnce(&mut Env) -> jni::errors::Result<T>,
) -> T {
    env.with_env(|env| {
        let result = operation(env);
        if result.is_err() {
            env.exception_clear();
        }
        result
    })
    .resolve::<LogErrorAndDefault>()
}

fn java_timezone_supported(env: &mut Env, id: &str) -> jni::errors::Result<bool> {
    let id_string = env.new_string(id)?;
    let zone = env
        .call_static_method(
            jni_str!("java/util/TimeZone"),
            jni_str!("getTimeZone"),
            jni_sig!("(Ljava/lang/String;)Ljava/util/TimeZone;"),
            &[JValue::Object(&id_string)],
        )?
        .l()?;
    let actual = env
        .call_method(
            &zone,
            jni_str!("getID"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    Ok(JString::cast_local(env, actual)?.try_to_string(env)? == id)
}

fn read_timezone_property(env: &mut Env) -> jni::errors::Result<String> {
    let key = env.new_string(TIMEZONE_PROPERTY)?;
    let value = env
        .call_static_method(
            jni_str!("android/os/SystemProperties"),
            jni_str!("get"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&key)],
        )?
        .l()?;
    JString::cast_local(env, value)?.try_to_string(env)
}

fn refresh_java_timezone(env: &mut Env, id: &str) -> jni::errors::Result<bool> {
    // Android 的默认时区来自 RuntimeInit 的属性 supplier，而非 user.timezone。
    // 这里只同步 user.timezone 供显式读取使用，实际默认值仍从私有属性重建。
    let key = env.new_string("user.timezone")?;
    let value = env.new_string(id)?;
    env.call_static_method(
        jni_str!("java/lang/System"),
        jni_str!("setProperty"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
        &[JValue::Object(&key), JValue::Object(&value)],
    )?;
    // Android 的实现同时通知 ICU4J 清除默认时区缓存；bindApplication 和
    // updateTimeZone 以后再调用 setDefault(null)，仍会读到进程内的伪装属性。
    env.call_static_method(
        jni_str!("java/util/TimeZone"),
        jni_str!("setDefault"),
        jni_sig!("(Ljava/util/TimeZone;)V"),
        &[JValue::Object(&JObject::null())],
    )?;
    Ok(true)
}

/// 在应用代码运行前更新进程环境，刷新 zygote 继承的 libc 本地时间状态。
#[cfg(target_os = "android")]
fn refresh_native_timezone(id: &str) -> anyhow::Result<()> {
    let id = std::ffi::CString::new(id)?;
    // SAFETY: tzset 是 bionic 的无参 C 函数；动态解析避免增加平台链接依赖。
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"tzset".as_ptr()) };
    ensure!(!symbol.is_null(), "bionic tzset unavailable");
    let tzset: unsafe extern "C" fn() = unsafe { std::mem::transmute(symbol) };
    // SAFETY: 仅在 fork 后的 pre_app_specialize 阶段操作，应用线程尚未启动；
    // setenv 复制字符串，不依赖模块卸载后的内存。IANA ID 没有 GMT 偏移符号歧义。
    ensure!(
        unsafe { libc::setenv(c"TZ".as_ptr(), id.as_ptr(), 1) } == 0,
        "setenv(TZ) failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe { tzset() };
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn refresh_native_timezone(_id: &str) -> anyhow::Result<()> {
    Ok(())
}

pub fn spoof_timezone(env: &mut EnvUnowned, id: &str, hide_maps: &[String]) -> anyhow::Result<()> {
    ensure!(valid_timezone_id(id), "invalid IANA timezone ID: {id}");
    ensure!(
        try_jni(env, |env| java_timezone_supported(env, id)),
        "timezone not supported by device tzdata: {id}"
    );

    let props = HashMap::from([(TIMEZONE_PROPERTY.to_string(), id.to_string())]);
    let explicit = HashSet::from([TIMEZONE_PROPERTY.to_string()]);
    // 始终走进程内副本，绝不降级到 companion/resetprop。
    cow_props::apply_cow_spoof(&props, &explicit)?;
    cow_props::apply_unreadable_ctx_synth(&props, &explicit, hide_maps)?;

    // COW 引擎可能跳过不可读的区或只记录 patch 错误，必须回读确认，
    // 不能仅以返回 Ok 判定成功后让 Java 与 native 各用一个时区。
    let actual = try_jni(env, read_timezone_property);
    if actual != id {
        bail!("process timezone property mismatch: requested {id}, got {actual:?}");
    }
    refresh_native_timezone(id)?;
    ensure!(
        try_jni(env, |env| refresh_java_timezone(env, id)),
        "failed to clear Java timezone cache"
    );
    info!("Process timezone faked: {id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::valid_timezone_id;

    #[test]
    fn accepts_iana_regions_and_utc() {
        for id in [
            "America/New_York",
            "Asia/Shanghai",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
            "UTC",
            "GMT",
        ] {
            assert!(valid_timezone_id(id), "{id}");
        }
    }

    #[test]
    fn rejects_offsets_markers_and_paths() {
        for id in [
            "",
            "__EMPTY__",
            "__DELETE__",
            "GMT+08:00",
            "+08:00",
            "../Asia/Shanghai",
            "/etc/localtime",
            "Asia//Tokyo",
            "Asia/Tokyo\0",
            "Asia/东京",
            " America/New_York ",
        ] {
            assert!(!valid_timezone_id(id), "{id:?}");
        }
    }
}
