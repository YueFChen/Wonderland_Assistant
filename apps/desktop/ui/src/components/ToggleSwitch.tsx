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
      className={`relative h-6 w-11 shrink-0 rounded-full transition focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-400 disabled:cursor-default disabled:opacity-50 ${checked ? 'bg-brand-500' : 'bg-glass-line-strong'}`}
    >
      <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-all ${checked ? 'left-[1.375rem]' : 'left-0.5'}`} />
    </button>
  )
}
