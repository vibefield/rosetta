export function ListActivityNotice({ pending, onShow, noun }: {
  readonly pending: boolean;
  readonly onShow: () => void;
  readonly noun: "projects" | "sessions";
}) {
  return (
    <>
      <span className="ah-list-update-status" role="status">
        {pending ? `New activity is available in ${noun}.` : ""}
      </span>
      <button
        type="button"
        className="ah-newpill ah-list-updates"
        data-list-updates=""
        data-visible={pending ? "" : undefined}
        hidden={!pending}
        onClick={onShow}
      >
        Show recent activity
      </button>
    </>
  );
}
