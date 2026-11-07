/** 与 Rust 的格式校验一致；时区是否存在以目标设备的 tzdata 为准。 */
export function isValidTimezoneId(value: string): boolean {
  const id = value.trim()
  if (!id || id === 'UTC' || id === 'GMT') return true
  return id.length < 92 && /^[A-Za-z][A-Za-z0-9_+-]*(\/[A-Za-z0-9_+-]+)+$/.test(id)
}
