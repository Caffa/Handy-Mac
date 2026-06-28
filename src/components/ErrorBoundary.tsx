import { Component, ErrorInfo, ReactNode } from "react";
import { useTranslation } from "react-i18next";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
  section?: string;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

class ErrorBoundaryClass extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo): void {
    // Log the error to console for debugging
    console.error(`ErrorBoundary caught an error${this.props.section ? ` in ${this.props.section}` : ""}:`, error);
    console.error("Component stack:", errorInfo.componentStack);

    // Call the optional error handler
    if (this.props.onError) {
      this.props.onError(error, errorInfo);
    }
  }

  handleRetry = (): void => {
    this.setState({ hasError: false, error: null });
  };

  render(): ReactNode {
    if (this.state.hasError) {
      // Use custom fallback if provided
      if (this.props.fallback) {
        return this.props.fallback;
      }

      // Default fallback UI
      return (
        <ErrorFallbackUI
          error={this.state.error}
          section={this.props.section}
          onRetry={this.handleRetry}
        />
      );
    }

    return this.props.children;
  }
}

// Separate functional component for the fallback UI
// This allows us to use hooks like useTranslation
function ErrorFallbackUI({
  error,
  section,
  onRetry,
}: {
  error: Error | null;
  section?: string;
  onRetry: () => void;
}): ReactNode {
  const { t } = useTranslation();

  const sectionLabel = section ? ` (${section})` : "";

  return (
    <div className="flex flex-col items-center justify-center p-8 min-h-[200px] bg-background border border-mid-gray/20 rounded-lg">
      <div className="flex items-center gap-3 mb-4 text-error">
        <svg
          className="w-6 h-6"
          fill="none"
          stroke="currentColor"
          viewBox="0 0 24 24"
        >
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
          />
        </svg>
        <h2 className="text-lg font-semibold">
          {t("errors.componentErrorTitle", "Something went wrong")}
          {sectionLabel}
        </h2>
      </div>

      <p className="text-mid-gray text-center mb-6 max-w-md">
        {t(
          "errors.componentErrorDescription",
          "An unexpected error occurred. The app will continue running, but this section may not work correctly.",
        )}
      </p>

      {error && (
        <details className="mb-4 w-full max-w-md">
          <summary className="cursor-pointer text-sm text-mid-gray hover:text-text transition-colors">
            {t("errors.viewDetails", "View error details")}
          </summary>
          <pre className="mt-2 p-3 bg-surface-secondary rounded text-xs overflow-auto max-h-32 text-error/80">
            {error.message}
            {"\n"}
            {error.stack?.split("\n").slice(0, 3).join("\n")}
          </pre>
        </details>
      )}

      <div className="flex gap-3">
        <button
          onClick={onRetry}
          className="px-4 py-2 bg-primary text-white rounded-lg hover:bg-primary/90 transition-colors"
        >
          {t("errors.tryAgain", "Try Again")}
        </button>
        <button
          onClick={() => window.location.reload()}
          className="px-4 py-2 bg-surface-secondary rounded-lg hover:bg-surface-secondary/80 transition-colors"
        >
          {t("errors.reloadApp", "Reload App")}
        </button>
      </div>

      <p className="mt-4 text-xs text-mid-gray">
        <a
          href="https://github.com/cjpais/Handy/issues"
          target="_blank"
          rel="noopener noreferrer"
          className="hover:text-primary underline"
        >
          {t("errors.reportIssue", "Report this issue on GitHub")}
        </a>
      </p>
    </div>
  );
}

// Wrapper function component that can use hooks
export function ErrorBoundary(props: Props): ReactNode {
  return <ErrorBoundaryClass {...props} />;
}

// Convenience wrapper for settings sections
export function SettingsErrorBoundary({
  children,
  section,
}: {
  children: ReactNode;
  section: string;
}): ReactNode {
  return (
    <ErrorBoundary section={section}>
      {children}
    </ErrorBoundary>
  );
}

export default ErrorBoundary;