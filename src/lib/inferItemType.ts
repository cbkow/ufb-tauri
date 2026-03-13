export type ItemType = "shot" | "asset" | "posting" | "other";

const SHOT_PATTERNS = /^(shots?|vfx)$/i;
const ASSET_PATTERNS = /^(assets?)$/i;
const POSTING_PATTERNS = /^(postings?|deliverables?)$/i;

export function inferItemType(folderName: string): ItemType {
  if (SHOT_PATTERNS.test(folderName)) return "shot";
  if (ASSET_PATTERNS.test(folderName)) return "asset";
  if (POSTING_PATTERNS.test(folderName)) return "posting";
  return "other";
}
