import { getColumnDefs } from "./tauri";
import type { ColumnDefinition } from "./types";

export async function buildMergedColumnDefs(
  items: { jobPath: string; folderName: string }[]
): Promise<ColumnDefinition[]> {
  // Collect unique {jobPath, folderName} pairs
  const seen = new Set<string>();
  const pairs: { jobPath: string; folderName: string }[] = [];
  for (const item of items) {
    const key = `${item.jobPath}\0${item.folderName}`;
    if (!seen.has(key)) {
      seen.add(key);
      pairs.push({ jobPath: item.jobPath, folderName: item.folderName });
    }
  }

  // Fetch column defs for each pair in parallel
  const allDefs = await Promise.all(
    pairs.map((p) => getColumnDefs(p.jobPath, p.folderName).catch(() => [] as ColumnDefinition[]))
  );

  // Merge by column name: first-seen definition wins; union dropdown options
  const merged = new Map<string, ColumnDefinition>();
  for (const defs of allDefs) {
    for (const col of defs) {
      const existing = merged.get(col.columnName);
      if (!existing) {
        merged.set(col.columnName, { ...col });
      } else {
        // Union options for dropdown/priority columns
        if (col.options?.length) {
          const existingNames = new Set(existing.options.map((o) => o.name));
          for (const opt of col.options) {
            if (!existingNames.has(opt.name)) {
              existing.options.push(opt);
            }
          }
        }
      }
    }
  }

  return Array.from(merged.values()).sort((a, b) => a.columnOrder - b.columnOrder);
}
