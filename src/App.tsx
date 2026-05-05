import { createContext, useContext, useState } from "react";
import { HashRouter, Routes, Route, Navigate } from "react-router-dom";
import { Login } from "./pages/Login";
import { Home } from "./pages/Home";
import { Config } from "./pages/Config";

interface AppState {
  isAuthenticated: boolean;
  setAuthenticated: (v: boolean) => void;
  activeProfileId: string | null;
  setActiveProfileId: (id: string | null) => void;
}

const AppContext = createContext<AppState>({
  isAuthenticated: false,
  setAuthenticated: () => {},
  activeProfileId: null,
  setActiveProfileId: () => {},
});

export const useAppContext = () => useContext(AppContext);

function ProtectedRoute({ children }: { children: React.ReactNode }) {
  const { isAuthenticated } = useAppContext();
  if (!isAuthenticated) return <Navigate to="/" replace />;
  return <>{children}</>;
}

function App() {
  const [isAuthenticated, setAuthenticated] = useState(false);
  const [activeProfileId, setActiveProfileId] = useState<string | null>(null);

  return (
    <AppContext.Provider
      value={{
        isAuthenticated,
        setAuthenticated,
        activeProfileId,
        setActiveProfileId,
      }}
    >
      <HashRouter>
        <Routes>
          <Route path="/" element={<Login />} />
          <Route
            path="/home"
            element={
              <ProtectedRoute>
                <Home />
              </ProtectedRoute>
            }
          />
          <Route
            path="/home/:strategySlug"
            element={
              <ProtectedRoute>
                <Home />
              </ProtectedRoute>
            }
          />
          <Route path="/dashboard" element={<Navigate to="/home" replace />} />
          <Route
            path="/settings"
            element={
              <ProtectedRoute>
                <Config />
              </ProtectedRoute>
            }
          />
          <Route
            path="/settings/create-wallet"
            element={<Navigate to="/settings" replace />}
          />
          <Route path="/config" element={<Navigate to="/settings" replace />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </HashRouter>
    </AppContext.Provider>
  );
}

export default App;
