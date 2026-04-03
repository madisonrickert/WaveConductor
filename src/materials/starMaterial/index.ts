import * as THREE from 'three';
import starImg from './star.png';

const starTexture = new THREE.TextureLoader().load(starImg);
starTexture.minFilter = THREE.NearestFilter;

export const starMaterial = new THREE.PointsMaterial({
  size: 13,
  sizeAttenuation: false,
  map: starTexture,
  opacity: 1,
  transparent: true,
  vertexColors: true,
});