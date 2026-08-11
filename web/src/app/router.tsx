import { createBrowserRouter } from 'react-router-dom';

import { FriendsRoute } from './routes/friends-route';
import { HomeRoute } from './routes/home-route';
import { LandingRoute } from './routes/landing-route';
import { OnboardingRoute } from './routes/onboarding-route';
import { PresenterRoute } from './routes/presenter-route';
import { RootRoute } from './routes/root-route';
import { SettingsRoute } from './routes/settings-route';
import { ShellRoute } from './routes/shell-route';
import { ViewerRoute } from './routes/viewer-route';

export const router = createBrowserRouter([
  {
    element: <RootRoute />,
    children: [
      {
        path: '/',
        element: <ShellRoute />,
        children: [
          { index: true, element: <HomeRoute /> },
          { path: 'friends', element: <FriendsRoute /> },
          { path: 'settings', element: <SettingsRoute /> },
        ],
      },
      { path: '/onboarding', element: <OnboardingRoute /> },
      { path: '/welcome', element: <LandingRoute /> },
      { path: '/present/:roomId', element: <PresenterRoute /> },
      { path: '/r/:roomId', element: <ViewerRoute /> },
    ],
  },
]);
