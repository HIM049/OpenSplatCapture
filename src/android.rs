use jni::{
    JNIEnv, JavaVM,
    errors::Result,
    objects::{JClass, JObject, JValue},
};

pub const SCENE_PERMISSION: &str = "com.oculus.permission.USE_SCENE";
pub const CAMERA_PERMISSION: &str = "android.permission.CAMERA";
pub const HEADSET_CAMERA_PERMISSION: &str = "horizonos.permission.HEADSET_CAMERA";
pub const ALL_PERMISSIONS: [&str; 3] = [
    SCENE_PERMISSION,
    CAMERA_PERMISSION,
    HEADSET_CAMERA_PERMISSION,
];

/// 把当前线程挂到 JVM 上执行闭包，闭包拿到 JNI 环境和 Activity；闭包内产生的本地引用在返回时统一释放。
pub fn with_activity<R>(f: impl FnOnce(&JNIEnv, JObject) -> Result<R>) -> Result<R> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }?;
    let env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    env.push_local_frame(64)?;
    let result = f(&env, activity);
    env.pop_local_frame(JObject::null())?;
    result
}

/// 用应用自己的类加载器加载 APK 里的辅助类。
pub fn helper_class<'a>(env: &JNIEnv<'a>, activity: JObject<'a>, name: &str) -> Result<JClass<'a>> {
    let loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let class = env
        .call_method(
            loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(env.new_string(name)?.into())],
        )?
        .l()?;
    Ok(JClass::from(class))
}

/// 调用一个无参、返回字符串的 Java 方法。
pub fn call_string(env: &JNIEnv, object: JObject, method: &str) -> Result<String> {
    let value = env
        .call_method(object, method, "()Ljava/lang/String;", &[])?
        .l()?;
    Ok(env.get_string(value.into())?.into())
}

/// 系统当前语言的代码，例如 "zh"、"en"。
pub fn system_language() -> Option<String> {
    with_activity(|env, _| {
        let locale = env
            .call_static_method(
                "java/util/Locale",
                "getDefault",
                "()Ljava/util/Locale;",
                &[],
            )?
            .l()?;
        call_string(env, locale, "getLanguage")
    })
    .ok()
}

/// 当前 Activity 是否已被授予指定权限。
pub fn has_permission(permission: &str) -> bool {
    with_activity(|env, activity| {
        let name = env.new_string(permission)?;
        let granted = env
            .call_method(
                activity,
                "checkSelfPermission",
                "(Ljava/lang/String;)I",
                &[JValue::Object(name.into())],
            )?
            .i()?;
        Ok(granted == 0)
    })
    .unwrap_or(false)
}

/// 一次性弹出系统权限申请对话框，结果需要之后用 `has_permission` 轮询。
pub fn request_permissions(permissions: &[&str]) -> Result<()> {
    with_activity(|env, activity| {
        let array = env.new_object_array(
            permissions.len() as i32,
            "java/lang/String",
            JObject::null(),
        )?;
        for (i, permission) in permissions.iter().enumerate() {
            env.set_object_array_element(array, i as i32, env.new_string(permission)?)?;
        }
        let array = unsafe { JObject::from_raw(array) };
        env.call_method(
            activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(array), JValue::Int(0)],
        )?;
        Ok(())
    })
}
