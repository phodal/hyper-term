export type HorizontalTabKey =
  | "ArrowLeft"
  | "ArrowRight"
  | "Home"
  | "End";

export function nextHorizontalTabIndex(
  itemCount: number,
  currentIndex: number,
  key: string,
): number | undefined {
  if (itemCount < 1) return undefined;
  const current = currentIndex >= 0 && currentIndex < itemCount
    ? currentIndex
    : 0;
  switch (key as HorizontalTabKey) {
    case "ArrowLeft":
      return (current - 1 + itemCount) % itemCount;
    case "ArrowRight":
      return (current + 1) % itemCount;
    case "Home":
      return 0;
    case "End":
      return itemCount - 1;
    default:
      return undefined;
  }
}
