import { createBrowserRouter } from 'react-router-dom';

import { HomeRoute } from './routes/home-route';
import { PresenterRoute } from './routes/presenter-route';
import { ViewerRoute } from './routes/viewer-route';

export const router = createBrowserRouter([
  { path: '/', element: <HomeRoute /> },
  { path: '/present/:roomId', element: <PresenterRoute /> },
  { path: '/r/:roomId', element: <ViewerRoute /> },
]);
