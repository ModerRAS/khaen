// 构建期常量（vite define 注入）：模板里必须经由 setup 常量引用，
// 直接用 __APP_VERSION__ 会被 Vue 模板编译器当成组件属性（undefined）。
export const APP_VERSION = __APP_VERSION__
export const APP_BUILD_TIME = __APP_BUILD_TIME__
export const APP_BUILD_ID = __APP_BUILD_ID__
