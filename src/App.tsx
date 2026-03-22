import { createContext, useContext, useState } from "react";
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { Login } from "./pages/Login";
import { Dashboard } from "./pages/Dashboard";
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
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<Login />} />
          <Route
            path="/dashboard"
            element={
              <ProtectedRoute>
                <Dashboard />
              </ProtectedRoute>
            }
          />
          <Route
            path="/config"
            element={
              <ProtectedRoute>
                <Config />
              </ProtectedRoute>
            }
          />
        </Routes>
      </BrowserRouter>
    </AppContext.Provider>
  );
}

export default App;
