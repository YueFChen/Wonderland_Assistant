type ToggleSwitchProps = {
  checked: boolean
  disabled?: boolean
  label: string
  onChange: (checked: boolean) => void
}

/** Shared compact switch for settings and plugin controls. */
export function ToggleSwitch({ checked, disabled = false, label, onChange }: ToggleSwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="app-toggle"
    >
      <span aria-hidden />
    </button>
  )
}
