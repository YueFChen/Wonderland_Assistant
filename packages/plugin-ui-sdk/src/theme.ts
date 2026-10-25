/** Component colors only; no background image, device path, geometry or plugin content colors. */
export const HOST_THEME_VARIABLES = [
  '--app-ink', '--app-ink-muted', '--app-ink-faint', '--app-ink-rgb', '--app-surface-rgb',
  '--app-glass', '--app-glass-subtle', '--app-glass-hover', '--app-glass-line', '--app-glass-line-strong',
  '--app-field', '--app-field-border', '--app-dialog-surface', '--app-dialog-backdrop', '--app-dialog-shadow',
  '--app-sidebar-surface', '--app-sidebar-shadow', '--app-sidebar-scrim',
  '--app-accent', '--app-accent-ink', '--app-accent-soft', '--app-accent-line', '--app-primary-ink',
  '--app-ornament', '--app-satin', '--app-sheen', '--app-page', '--app-title-from', '--app-title-to',
  '--app-danger', '--app-danger-soft', '--app-danger-line',
  '--app-success', '--app-success-soft', '--app-success-line',
  '--app-warning', '--app-warning-soft', '--app-warning-line',
] as const

export interface HostTheme {
  resolved: 'light' | 'dark'
  /** Optional in Bridge 1.1; older hosts continue to use the shared preset colors. */
  variables?: Partial<Record<typeof HOST_THEME_VARIABLES[number], string>>
}

export function applyHostThemeVariables(style: Pick<CSSStyleDeclaration, 'setProperty' | 'removeProperty'>, variables: HostTheme['variables']) {
  for (const property of HOST_THEME_VARIABLES) {
    const value = variables?.[property]
    if (typeof value === 'string' && value.length <= 256) style.setProperty(property, value)
    else style.removeProperty(property)
  }
}
