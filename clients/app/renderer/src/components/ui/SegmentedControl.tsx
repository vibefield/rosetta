import type { ReactNode } from "react";

export interface SegmentedControlOption<Value extends string> {
  readonly value: Value;
  readonly label: ReactNode;
  readonly accessibleLabel?: string;
  readonly disabled?: boolean;
}

export interface SegmentedControlProps<Value extends string> {
  readonly value: Value;
  readonly options: readonly SegmentedControlOption<Value>[];
  readonly accessibleLabel: string;
  readonly onValueChange: (value: Value) => void;
  readonly className?: string;
}

/** A controlled, mutually exclusive group of pressed buttons. */
export function SegmentedControl<Value extends string>({
  value,
  options,
  accessibleLabel,
  onValueChange,
  className,
}: SegmentedControlProps<Value>) {
  const classes = className ? `ah-seg ${className}` : "ah-seg";

  return (
    <div className={classes} role="group" aria-label={accessibleLabel}>
      {options.map((option) => {
        const isSelected = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            className={isSelected ? "is-on" : ""}
            aria-label={option.accessibleLabel}
            aria-pressed={isSelected}
            disabled={option.disabled}
            onClick={() => onValueChange(option.value)}
          >
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
