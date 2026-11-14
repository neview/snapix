/**
 * 晾衣绳目录的「入场动画」判定。
 *
 * 后端把新图挂到绳上时先置 `pending`（位置已定但先不显示），等飞行动画落地才摘掉；
 * 前端据此判断哪些条目需要播一次「从绳上垂下」的入场动画。
 *
 * 关键一条：**飞过来的那张不能播**。它已经精确停在落点上，再播一次位移 + 缩放，
 * 屏幕上就是一次可见的弹跳（用户反馈的「截图到绳子后弹一下」）。
 */
export type FreshItem = { id: string; pending?: boolean };

/**
 * 挑出这一轮需要播入场动画的条目 id。
 *
 * @param prev    上一轮可见的 id 集合
 * @param items   本轮全部条目（含 pending）
 * @param landed  刚刚「飞过来 / 挂回来」的 id 集合
 */
export function pickFreshIds(
  prev: ReadonlySet<string>,
  items: readonly FreshItem[],
  landed: ReadonlySet<string>,
): string[] {
  const out: string[] = [];
  for (const item of items) {
    if (item.pending) continue;
    if (prev.has(item.id)) continue;
    if (landed.has(item.id)) continue;
    out.push(item.id);
  }
  return out;
}
