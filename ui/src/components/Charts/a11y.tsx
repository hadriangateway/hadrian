import type { ReactNode } from "react";

export interface ChartA11yColumn {
  /** Column header text */
  header: string;
  /** Render the cell value for a given row */
  render: (row: Record<string, unknown>, index: number) => string | number | null | undefined;
}

export interface ChartA11yProps {
  /** Short description of the chart for screen readers (used as aria-label / caption). */
  ariaLabel: string;
  /** Underlying chart data — used to render the SR-only data table. */
  data: ReadonlyArray<Record<string, unknown>>;
  /** Column definitions for the SR-only table. */
  columns: ChartA11yColumn[];
  /** The visual chart (typically a `<ResponsiveContainer>` tree). */
  children: ReactNode;
  /** Hard cap on the number of rows rendered into the SR table to avoid blowing up
   * the accessibility tree on very large series. Defaults to 200; charts that
   * pass more rows than this cap are summarised with a trailing "+N more rows". */
  maxRows?: number;
}

/**
 * Wraps a recharts visual with a `role="img"` figure + an `aria-label` and a
 * visually-hidden `<table>` so assistive tech can read out the underlying data
 * (recharts only emits SVG, which is invisible to screen readers).
 *
 * Mirrors the pattern recommended by the WAI Charts authoring practices.
 */
export function ChartA11y({ ariaLabel, data, columns, children, maxRows = 200 }: ChartA11yProps) {
  const visibleRows = data.slice(0, maxRows);
  const truncated = data.length > visibleRows.length;

  return (
    <figure role="img" aria-label={ariaLabel} className="relative m-0 w-full">
      {children}
      <div className="sr-only">
        <table>
          <caption>{ariaLabel}</caption>
          <thead>
            <tr>
              {columns.map((col) => (
                <th key={col.header} scope="col">
                  {col.header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {visibleRows.map((row, index) => (
              <tr key={index}>
                {columns.map((col) => {
                  const value = col.render(row, index);
                  return (
                    <td key={col.header}>
                      {value === null || value === undefined ? "" : String(value)}
                    </td>
                  );
                })}
              </tr>
            ))}
            {truncated && (
              <tr>
                <td colSpan={columns.length}>
                  +{data.length - visibleRows.length} more rows truncated for accessibility
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </figure>
  );
}

/**
 * Recharts becomes increasingly painful to navigate at large data sizes (every
 * point becomes a tiny tap target and the SVG layout cost scales linearly).
 * For long date ranges, callers should bucket their data — this helper is a
 * simple LTTB-ish "every Nth point" downsampler that always keeps the first
 * and last rows so the rendered axis still spans the requested range.
 */
export function downsampleForChart<T>(data: ReadonlyArray<T>, maxPoints: number): T[] {
  if (data.length <= maxPoints || maxPoints < 2) {
    return data.slice();
  }
  const step = (data.length - 1) / (maxPoints - 1);
  const out: T[] = [];
  for (let i = 0; i < maxPoints - 1; i++) {
    out.push(data[Math.round(i * step)]);
  }
  out.push(data[data.length - 1]);
  return out;
}
