import { classifyCandidatePair } from './candidate-pair-classifier';

describe('candidate pair classification', () => {
  it('classifies relay when either selected candidate is relayed', () => {
    expect(classifyCandidatePair([
      { id: 'transport', type: 'transport', selectedCandidatePairId: 'pair' },
      { id: 'pair', type: 'candidate-pair', localCandidateId: 'local', remoteCandidateId: 'remote' },
      { id: 'local', type: 'local-candidate', candidateType: 'relay' },
      { id: 'remote', type: 'remote-candidate', candidateType: 'srflx' },
    ]).path).toBe('relay');
  });

  it('does not expose addresses and handles missing reports', () => {
    expect(classifyCandidatePair([])).toEqual({ path: 'unavailable' });
    expect(JSON.stringify(classifyCandidatePair([]))).not.toContain('address');
  });
});
