import { Component, type ErrorInfo, type ReactNode } from 'react';

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/** Evita a "tela preta": um erro de render mostra a mensagem em vez de desmontar
 *  a árvore React inteira (comum quando o daemon é uma versão antiga cujo JSON
 *  não tem um campo que o frontend espera). */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('ErrorBoundary:', error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 32, color: '#e6edf3', fontFamily: 'inherit' }}>
          <h1 style={{ fontSize: 18 }}>Algo quebrou nesta tela 😵</h1>
          <p style={{ color: '#8b949e' }}>
            Provável incompatibilidade com a versão do daemon — tente reconstruir e
            reiniciar o daemon. Detalhe do erro:
          </p>
          <pre
            style={{
              background: '#161b22',
              border: '1px solid #2a313c',
              borderRadius: 8,
              padding: 12,
              overflow: 'auto',
              color: '#f85149',
              fontSize: 12,
            }}
          >
            {this.state.error.message}
          </pre>
          <button
            onClick={() => this.setState({ error: null })}
            style={{
              background: '#2f81f7',
              border: 'none',
              color: '#fff',
              padding: '8px 14px',
              borderRadius: 8,
              cursor: 'pointer',
            }}
          >
            ↻ Tentar de novo
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
