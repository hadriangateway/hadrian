import { formatApiError } from "@/utils/formatApiError";
/**
 * Pyodide Web Worker
 *
 * This worker loads and manages a Pyodide instance for executing Python code
 * in a sandboxed environment. Running Pyodide in a worker prevents blocking
 * the main thread during heavy computation.
 *
 * Communication protocol:
 * - Main thread sends { type, id, ... } messages
 * - Worker responds with { type, id, ... } messages
 * - Errors are sent as { type: "error", id, error: string }
 */

// Pyodide interface type (we load from CDN, not from npm)
interface PyProxy {
  toJs(): unknown;
  destroy(): void;
  install(pkg: string): Promise<void>;
}

interface PyodideInterface {
  runPython(code: string, options?: { globals?: unknown; locals?: unknown }): unknown;
  runPythonAsync(code: string, options?: { globals?: unknown; locals?: unknown }): Promise<unknown>;
  loadPackage(names: string | string[]): Promise<unknown>;
  loadPackagesFromImports(code: string): Promise<unknown>;
  pyimport(name: string): PyProxy;
  globals: {
    get(name: string): PyProxy | undefined;
    set(name: string, value: unknown): void;
  };
  setStdout(options: { batched?: (msg: string) => void }): void;
  setStderr(options: { batched?: (msg: string) => void }): void;
}

// Pyodide runtime assets are self-hosted same-origin (vendored by
// scripts/vendor-wasm.mjs into ui/public/wasm/pyodide/) so the CSP needs no
// third-party script-src. Overridable at build time to point back at a CDN.
const PYODIDE_BASE_URL = import.meta.env.VITE_PYODIDE_BASE_URL ?? "/wasm/pyodide/";

/** Message types from main thread to worker */
interface ExecuteMessage {
  type: "execute";
  id: string;
  code: string;
  packages?: string[];
}

interface LoadPackagesMessage {
  type: "loadPackages";
  id: string;
  packages: string[];
}

interface StatusMessage {
  type: "status";
  id: string;
}

type WorkerMessage = ExecuteMessage | LoadPackagesMessage | StatusMessage;

/** Message types from worker to main thread */
interface ReadyResponse {
  type: "ready";
}

interface LoadingResponse {
  type: "loading";
  stage: "pyodide" | "packages";
  message: string;
}

interface ExecuteResponse {
  type: "executeResult";
  id: string;
  success: boolean;
  result?: unknown;
  stdout: string;
  stderr: string;
  figures: string[]; // Base64 encoded PNG images from matplotlib
  error?: string;
}

interface PackagesLoadedResponse {
  type: "packagesLoaded";
  id: string;
  packages: string[];
}

interface StatusResponse {
  type: "statusResult";
  id: string;
  ready: boolean;
  loadedPackages: string[];
}

interface ErrorResponse {
  type: "error";
  id?: string;
  error: string;
}

type WorkerResponse =
  | ReadyResponse
  | LoadingResponse
  | ExecuteResponse
  | PackagesLoadedResponse
  | StatusResponse
  | ErrorResponse;

// Worker state
let pyodide: PyodideInterface | null = null;
let isLoading = false;
const loadedPackages = new Set<string>();

/**
 * Send a message to the main thread
 */
function sendMessage(message: WorkerResponse) {
  self.postMessage(message);
}

/**
 * Initialize Pyodide by loading the self-hosted runtime
 */
async function initPyodide(): Promise<PyodideInterface> {
  if (pyodide) return pyodide;
  if (isLoading) {
    // Wait for existing load
    while (isLoading) {
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    if (pyodide) return pyodide;
    throw new Error("Pyodide initialization failed");
  }

  isLoading = true;
  sendMessage({ type: "loading", stage: "pyodide", message: "Loading Python runtime..." });

  try {
    // Dynamically import the same-origin Pyodide loader.
    const pyodideModule = await import(/* @vite-ignore */ `${PYODIDE_BASE_URL}pyodide.mjs`);

    const py: PyodideInterface = await pyodideModule.loadPyodide({
      indexURL: PYODIDE_BASE_URL,
    });

    // Load matplotlib first (required before importing)
    sendMessage({
      type: "loading",
      stage: "packages",
      message: "Loading matplotlib for visualization support...",
    });
    await py.loadPackage("matplotlib");
    loadedPackages.add("matplotlib");

    // Set up matplotlib backend for headless rendering
    await py.runPythonAsync(`
import sys
import io

# Configure matplotlib for Agg backend (headless)
import matplotlib
matplotlib.use('Agg')

# Helper function to capture figures as base64
def __hadrian_get_figures():
    import matplotlib.pyplot as plt
    import base64
    import io

    figures = []
    for fig_num in plt.get_fignums():
        fig = plt.figure(fig_num)
        buf = io.BytesIO()
        fig.savefig(buf, format='png', dpi=100, bbox_inches='tight')
        buf.seek(0)
        img_data = base64.b64encode(buf.read()).decode('utf-8')
        figures.append(img_data)
        plt.close(fig)

    return figures
`);

    pyodide = py;
    sendMessage({ type: "ready" });
    return py;
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    sendMessage({ type: "error", error: `Failed to load Pyodide: ${errorMsg}` });
    throw error;
  } finally {
    isLoading = false;
  }
}

/**
 * Load Python packages
 */
async function loadPackages(packages: string[]): Promise<string[]> {
  const py = await initPyodide();

  // Filter out already loaded packages
  const toLoad = packages.filter((pkg) => !loadedPackages.has(pkg));
  if (toLoad.length === 0) return packages;

  sendMessage({
    type: "loading",
    stage: "packages",
    message: `Loading packages: ${toLoad.join(", ")}...`,
  });

  // Load packages using micropip for pure Python packages
  // and loadPackage for packages with C extensions
  const cExtPackages = ["numpy", "pandas", "scipy", "matplotlib", "scikit-learn", "pillow"];
  const cExt = toLoad.filter((pkg) => cExtPackages.includes(pkg.toLowerCase()));
  const pure = toLoad.filter((pkg) => !cExtPackages.includes(pkg.toLowerCase()));

  if (cExt.length > 0) {
    await py.loadPackage(cExt);
  }

  if (pure.length > 0) {
    await py.loadPackage("micropip");
    const micropip = py.pyimport("micropip");
    for (const pkg of pure) {
      try {
        await micropip.install(pkg);
      } catch {
        // Package might not be available via micropip, try loadPackage
        try {
          await py.loadPackage(pkg);
        } catch {
          console.warn(`Failed to load package: ${pkg}`);
        }
      }
    }
  }

  toLoad.forEach((pkg) => loadedPackages.add(pkg));
  return packages;
}

/**
 * Execute Python code and capture output
 */
async function executeCode(
  code: string,
  packages?: string[]
): Promise<{
  success: boolean;
  result?: unknown;
  stdout: string;
  stderr: string;
  figures: string[];
  error?: string;
}> {
  const py = await initPyodide();

  // Load any required packages
  if (packages && packages.length > 0) {
    await loadPackages(packages);
  }

  // Also auto-detect imports and load packages
  try {
    await py.loadPackagesFromImports(code);
  } catch {
    // Ignore errors from import detection
  }

  // Capture stdout and stderr
  let stdout = "";
  let stderr = "";

  py.setStdout({
    batched: (msg: string) => {
      stdout += msg + "\n";
    },
  });

  py.setStderr({
    batched: (msg: string) => {
      stderr += msg + "\n";
    },
  });

  try {
    // Clear any existing figures before execution
    await py.runPythonAsync(`
import matplotlib.pyplot as plt
plt.close('all')
`);

    // Execute the code
    const result = await py.runPythonAsync(code);

    // Get any generated figures
    let figures: string[] = [];
    const getFigures = py.globals.get("__hadrian_get_figures");
    if (getFigures) {
      const figuresProxy = (getFigures as unknown as () => PyProxy)();
      figures = figuresProxy.toJs() as string[];
      figuresProxy.destroy();
    }

    // Convert result to JavaScript
    let jsResult: unknown = undefined;
    if (result !== undefined && result !== null) {
      const resultProxy = result as PyProxy;
      if (typeof resultProxy.toJs === "function") {
        jsResult = resultProxy.toJs();
        resultProxy.destroy();
      } else {
        jsResult = result;
      }
    }

    return {
      success: true,
      result: jsResult,
      stdout: stdout.trim(),
      stderr: stderr.trim(),
      figures,
    };
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : formatApiError(error);
    return {
      success: false,
      stdout: stdout.trim(),
      stderr: stderr.trim(),
      figures: [],
      error: errorMsg,
    };
  }
}

/**
 * Handle messages from the main thread
 */
self.onmessage = async (event: MessageEvent<WorkerMessage>) => {
  const message = event.data;

  switch (message.type) {
    case "execute": {
      try {
        const result = await executeCode(message.code, message.packages);
        sendMessage({
          type: "executeResult",
          id: message.id,
          ...result,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "loadPackages": {
      try {
        const loaded = await loadPackages(message.packages);
        sendMessage({
          type: "packagesLoaded",
          id: message.id,
          packages: loaded,
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : formatApiError(error);
        sendMessage({
          type: "error",
          id: message.id,
          error: errorMsg,
        });
      }
      break;
    }

    case "status": {
      sendMessage({
        type: "statusResult",
        id: message.id,
        ready: pyodide !== null,
        loadedPackages: Array.from(loadedPackages),
      });
      break;
    }

    default:
      sendMessage({
        type: "error",
        error: `Unknown message type: ${(message as { type: string }).type}`,
      });
  }
};

// Start loading Pyodide immediately
initPyodide().catch(console.error);
