// The fixtures' host. A fixture fills `client` with the client methods it
// fakes; `ports` hands them to the room through the same doors a real host
// uses. Anything left unfilled answers nothing — a promise that never settles,
// a subscription that never speaks — as an idle daemon would.
export const client = {};

const never = () => new Promise(() => {});
const call = (name, fallback) => (...args) =>
  typeof client[name] === "function" ? client[name](...args) : fallback(...args);

const sessionClient = {
  subscribeSession: call("subscribeSession", () => ({
    entries: () => [], fetchPage: never, reveal() {}, loadEntry: async () => false, close() {},
  })),
  subscribeCollaboration: call("subscribeCollaboration", () => ({ close() {}, restart() {} })),
  timelineEntry: call("timelineEntry", never),
  reportWork: call("reportWork", never),
  command: call("command", never),
  readFileTarget: call("readFileTarget", never),
  readPlan: call("readPlan", never),
  readTextAsset: call("readTextAsset", never),
  readAsset: call("readAsset", never),
};

export const ports = {
  catalog: {
    subscribeMeta: call("subscribeMeta", () => ({ close() {} })),
    status: call("status", never),
    onConnectionChange: call("onConnectionChange", () => () => {}),
    projectStats: call("projectStats", never),
    usage: call("usage", never),
    dayUsage: call("dayUsage", never),
    search: call("search", never),
    boost: call("boost", async () => ({})),
  },
  sessions: { open: () => sessionClient, close() {} },
  assets: {
    imageAssetUrl: call("imageAssetUrl", (asset) => asset.url),
    attachmentAssetUrl: call("attachmentAssetUrl", (asset) => asset.url),
  },
  host: null,
};
