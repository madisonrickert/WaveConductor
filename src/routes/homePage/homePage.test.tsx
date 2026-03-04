import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { HomePage } from './index';

describe('HomePage', () => {
  const renderHomePage = () =>
    render(
      <MemoryRouter>
        <HomePage />
      </MemoryRouter>
    );

  it('renders all five sketch thumbnails', () => {
    renderHomePage();
    expect(screen.getByAltText('Cymatics')).toBeInTheDocument();
    expect(screen.getByAltText('Line')).toBeInTheDocument();
    expect(screen.getByAltText('Flame')).toBeInTheDocument();
    expect(screen.getByAltText('Dots')).toBeInTheDocument();
    expect(screen.getByAltText('Waves')).toBeInTheDocument();
  });

  it('renders credits section', () => {
    renderHomePage();
    expect(screen.getByText('CharGallery')).toBeInTheDocument();
    expect(screen.getByText('Xiaohan Zhang')).toBeInTheDocument();
    expect(screen.getByText('Madison Rickert')).toBeInTheDocument();
  });

  it('renders a link to the licenses page', () => {
    renderHomePage();
    expect(screen.getByText('Open Source Licenses')).toBeInTheDocument();
  });
});
