import { tailStep } from "./tail-state";

export interface TailState {
  readonly away: boolean;
  readonly unseen: number;
}

/** One owner for live-follow and its animation. DOM scroll events observe
 * positions; wheel/touch/key intent can interrupt before the next frame. */
export class TailScroll {
  private following = true;
  private ready = false;
  private landed = false;
  private opening = false;
  private unseen = 0;
  private frame: number | null = null;
  private previousTime = 0;
  private returningToLatest = false;
  private writtenTop: number;
  private state: TailState = { away: false, unseen: 0 };
  private readonly element: HTMLElement;
  private readonly publish: (state: TailState) => void;
  private readonly reduced: () => boolean;

  constructor(element: HTMLElement, publish: (state: TailState) => void, reduced: () => boolean) {
    this.element = element;
    this.publish = publish;
    this.reduced = reduced;
    this.writtenTop = element.scrollTop;
    this.setFollowing(true);
  }

  get isFollowing(): boolean {
    return this.following;
  }

  get isAnimating(): boolean {
    return this.frame !== null;
  }

  private bottom(): number {
    return Math.max(0, this.element.scrollHeight - this.element.clientHeight);
  }

  private setFollowing(value: boolean): void {
    this.following = value;
    this.element.toggleAttribute("data-following", value);
  }

  private emit(): void {
    const away =
      this.ready &&
      !this.following &&
      (this.bottom() - this.element.scrollTop > 32 || this.unseen > 0);
    const unseen = this.following ? 0 : this.unseen;
    if (away === this.state.away && unseen === this.state.unseen) return;
    this.state = { away, unseen };
    this.publish(this.state);
  }

  private write(top: number): void {
    this.element.scrollTop = top;
    this.writtenTop = this.element.scrollTop;
  }

  private stop(): void {
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
    this.returningToLatest = false;
  }

  pause = (): void => {
    this.stop();
    this.setFollowing(false);
    this.emit();
  };

  onScroll = (): void => {
    if (!this.ready) return;
    const top = this.element.scrollTop;
    // During an explicit return, native PageUp/anchor easing may keep sending
    // positions for several frames. Fresh input and focus own interrupts;
    // those stale position events must not cancel the reader's latest click.
    if (!this.returningToLatest && Math.abs(top - this.writtenTop) > 1) {
      this.stop();
      this.setFollowing(false);
    }
    this.writtenTop = top;
    if (this.bottom() - top <= 8) {
      this.setFollowing(true);
      this.unseen = 0;
    }
    this.emit();
  };

  update(ready: boolean, arrivals: number, opening = false): void {
    this.opening = opening;
    this.ready = ready;
    if (!ready) {
      this.stop();
      this.emit();
      return;
    }
    this.unseen += arrivals;
    if (!this.landed) {
      this.landed = true;
      // Open a session at its latest message before paint. Search anchors
      // pause follow before reaching here and retain their own landing.
      if (this.following) this.write(this.bottom());
    } else {
      this.resize();
    }
    this.emit();
  }

  resize = (): void => {
    if (!this.ready) return;
    if (!this.following) {
      this.emit();
      return;
    }
    if (this.opening || this.reduced()) {
      this.stop();
      this.write(this.bottom());
      this.unseen = 0;
      this.emit();
    } else if (Math.abs(this.bottom() - this.element.scrollTop) <= 0.5) {
      this.stop();
      this.unseen = 0;
      this.emit();
    } else if (this.frame === null) {
      this.previousTime = performance.now();
      this.frame = requestAnimationFrame(this.tick);
    }
  };

  private tick = (time: number): void => {
    this.frame = null;
    if (!this.following || !this.ready) return;
    const target = this.bottom();
    const before = this.element.scrollTop;
    this.write(
      this.reduced() ? target : tailStep(before, target, time - this.previousTime),
    );
    // Browsers quantize scrollTop to device pixels. At high refresh rates a
    // subpixel step can round back to the same position forever. Make one
    // pixel of progress when that happens, bounded by the remaining distance.
    if (this.element.scrollTop === before && target !== before) {
      this.write(before + Math.sign(target - before) * Math.min(1, Math.abs(target - before)));
    }
    this.previousTime = time;
    if (Math.abs(target - this.element.scrollTop) <= 1) {
      this.write(target);
      this.returningToLatest = false;
      this.unseen = 0;
      this.emit();
    } else {
      this.frame = requestAnimationFrame(this.tick);
    }
  };

  latest = (): void => {
    this.stop();
    // PageUp/PageDown and anchor navigation can leave a native smooth scroll
    // in flight. Cancel it now, before its queued scroll event can interrupt
    // the first frame of this explicitly requested return to latest.
    this.element.scrollTo({ top: this.element.scrollTop, behavior: "instant" });
    this.writtenTop = this.element.scrollTop;
    this.returningToLatest = true;
    this.setFollowing(true);
    this.resize();
    if (this.bottom() - this.element.scrollTop <= 8) this.unseen = 0;
    this.emit();
  };

  dispose(): void {
    this.stop();
    this.element.removeAttribute("data-following");
  }
}
