import { AppHeader } from '@/components/layout/app-header';
import { CreateRoomForm } from '@/features/room-creation/components/create-room-form';

export function HomeRoute() {
  return (
    <div className="app-shell home-shell">
      <AppHeader assurance="Streams straight to your viewers" />
      <main className="home-content">
        <section className="home-main" aria-labelledby="home-title">
          <div className="home-intro">
            <p className="eyebrow">High-fidelity browser sharing</p>
            <h1 id="home-title">Share the pixels<br />that matter.</h1>
            <p className="home-intro__lede">
              Present detailed text, design work, or motion to up to ten viewers at full
              resolution. No install, no media server, no chat-app compression preset.
            </p>
            <div className="topology-line" aria-label="One presenter sends independent streams to up to ten invited viewers">
              <span>Presenter</span>
              <span className="topology-line__rule" aria-hidden="true" />
              <span className="topology-line__arrow" aria-hidden="true">→</span>
              <span>1–10 invited viewers</span>
            </div>
          </div>
          <CreateRoomForm />
        </section>

        <section className="landing-section" aria-labelledby="process-title">
          <div className="landing-section__inner">
            <p className="eyebrow">Process</p>
            <h2 id="process-title" className="landing-section__title">From link to live in under a minute.</h2>
            <div className="process-grid">
              <ProcessStep index="01" title="Create a room">
                Set the admission policy, viewer cap, and lifetime. Nothing about the room persists after it closes.
              </ProcessStep>
              <ProcessStep index="02" title="Send one link">
                A single secure URL. Viewers join directly, or wait in the lobby until you admit them.
              </ProcessStep>
              <ProcessStep index="03" title="Present at full resolution">
                Your screen streams peer to peer, unscaled and uncompressed, with each connection monitored live.
              </ProcessStep>
            </div>
          </div>
        </section>

        <section className="landing-section topology-section" aria-labelledby="topology-title">
          <div className="landing-section__inner">
            <p className="eyebrow">Topology</p>
            <h2 id="topology-title" className="landing-section__title">Why the direct path looks better.</h2>

            <figure className="network-diagram">
              <figcaption className="sr-only">
                The presenter connects directly to each viewer when possible, with an encrypted relay as a backup route.
              </figcaption>
              <div className="network-node">
                <span className="network-node__screen" aria-hidden="true" />
                <span>Presenter</span>
              </div>
              <div className="network-path" aria-hidden="true">
                <span>Direct connection · full resolution</span>
                <span className="network-path__line" />
                <span className="network-path__branch" />
                <span className="network-path__relay"><i />Backup route · used only if needed</span>
              </div>
              <div className="network-node">
                <span className="network-node__viewers" aria-hidden="true">
                  {Array.from({ length: 10 }, (_, index) => <i key={index} />)}
                </span>
                <span>Up to 10 viewers</span>
              </div>
            </figure>

            <div className="topology-grid">
              <TopologyPoint title="Direct first">
                Your browser connects straight to each viewer whenever the network allows it. This is the shortest path, with the least delay and no extra recompression.
              </TopologyPoint>
              <TopologyPoint title="Relay when needed">
                When a direct path is not possible, your stream uses an encrypted backup relay, with a little more delay and a lower quality ceiling.
              </TopologyPoint>
              <TopologyPoint title="Quality, visible">
                Every connection reports its path, bitrate, and frame health live, so you can see and act on a drop in fidelity.
              </TopologyPoint>
            </div>
          </div>
        </section>
      </main>
      <footer className="landing-footer">
        <span>Clarity Share: high-fidelity, install-free screen sharing.</span>
        <span>No sign-up required. Each room cleans itself up automatically once it closes.</span>
      </footer>
    </div>
  );
}

function ProcessStep({ index, title, children }: { index: string; title: string; children: string }) {
  return (
    <article>
      <span className="step-index" aria-hidden="true">{index}</span>
      <h3>{title}</h3>
      <p>{children}</p>
    </article>
  );
}

function TopologyPoint({ title, children }: { title: string; children: string }) {
  return (
    <article>
      <h3>{title}</h3>
      <p>{children}</p>
    </article>
  );
}
