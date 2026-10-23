export const HOME_SURFACE_FORMING = { delay: 0.5, duration: 0.42, ease: [0.19, 1, 0.22, 1] as [number, number, number, number] };
export const HOME_SURFACE_LEAVING = { duration: 0.36, ease: [0.4, 0, 1, 1] as [number, number, number, number] };

export function homeSurfaceFade(reducedMotion: boolean) {
  return {
    initial: { opacity: reducedMotion ? 1 : 0 },
    animate: { opacity: 1, transition: reducedMotion ? { duration: 0 } : HOME_SURFACE_FORMING },
    exit: { opacity: 0, transition: reducedMotion ? { duration: 0 } : HOME_SURFACE_LEAVING },
  };
}
