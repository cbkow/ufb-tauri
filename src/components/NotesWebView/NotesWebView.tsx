import "./NotesWebView.css";

interface NotesWebViewProps {
  url: string;
}

export function NotesWebView(props: NotesWebViewProps) {
  return (
    <div class="notes-webview">
      <iframe
        src={props.url}
        class="notes-iframe"
        allow="clipboard-write"
        sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox"
      />
    </div>
  );
}
