import { createSignal } from "solid-js";
import { listen } from "@tauri-apps/api/event";

export interface FileOperation {
  id: string;
  operation: string;
  totalBytes: number;
  copiedBytes: number;
  currentFile: string;
  itemsTotal: number;
  itemsDone: number;
  status: "active" | "completed" | "completed_with_errors" | "error";
  error?: string;
  errors?: string[];
  succeeded?: number;
  failed?: number;
}

function createFileOpsStore() {
  const [operations, setOperations] = createSignal<FileOperation[]>([]);

  listen<any>("fileop:started", (event) => {
    const { id, operation, itemsTotal } = event.payload;
    setOperations((ops) => [
      ...ops,
      {
        id,
        operation,
        totalBytes: 0,
        copiedBytes: 0,
        currentFile: "",
        itemsTotal: itemsTotal ?? 0,
        itemsDone: 0,
        status: "active",
      },
    ]);
  });

  listen<any>("fileop:progress", (event) => {
    const p = event.payload;
    setOperations((ops) =>
      ops.map((op) =>
        op.id === p.id
          ? {
              ...op,
              totalBytes: p.totalBytes ?? op.totalBytes,
              copiedBytes: p.copiedBytes ?? op.copiedBytes,
              currentFile: p.currentFile ?? op.currentFile,
              itemsTotal: p.itemsTotal ?? op.itemsTotal,
              itemsDone: p.itemsDone ?? op.itemsDone,
            }
          : op
      )
    );
  });

  listen<any>("fileop:completed", (event) => {
    const { id, errors, succeeded, failed } = event.payload;
    const hasErrors = errors && errors.length > 0;
    setOperations((ops) =>
      ops.map((op) =>
        op.id === id
          ? {
              ...op,
              status: hasErrors ? "completed_with_errors" as const : "completed" as const,
              errors: errors ?? undefined,
              succeeded: succeeded ?? op.itemsTotal,
              failed: failed ?? 0,
            }
          : op
      )
    );
    // Auto-dismiss only fully successful operations
    if (!hasErrors) {
      setTimeout(() => {
        setOperations((ops) => ops.filter((op) => op.id !== id));
      }, 3000);
    }
  });

  listen<any>("fileop:error", (event) => {
    const { id, error } = event.payload;
    setOperations((ops) =>
      ops.map((op) =>
        op.id === id
          ? { ...op, status: "error" as const, error }
          : op
      )
    );
  });

  function dismiss(id: string) {
    setOperations((ops) => ops.filter((op) => op.id !== id));
  }

  return { operations, dismiss };
}

export const fileOpsStore = createFileOpsStore();
