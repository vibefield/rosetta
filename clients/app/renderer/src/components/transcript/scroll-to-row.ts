/** Scroll only the transcript, never its overflow-hidden room ancestors. */
export function scrollRowTo(scroller: HTMLElement, element: HTMLElement, block: "center" | "nearest"): void {
  const frame = scroller.getBoundingClientRect();
  const rect = element.getBoundingClientRect();
  const top = rect.top - frame.top + scroller.scrollTop;
  if (block === "center") {
    scroller.scrollTop = top - Math.max(0, (scroller.clientHeight - rect.height) / 2);
  } else if (rect.top < frame.top) scroller.scrollTop = top;
  else if (rect.bottom > frame.bottom) scroller.scrollTop = top - (scroller.clientHeight - rect.height);
}
