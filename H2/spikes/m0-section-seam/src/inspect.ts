import { generateCorpus, countWords } from './corpus.js'

const c = generateCorpus({ targetWords: 200_000, wordsPerSection: 1500 })
let blocks = 0
let textNodes = 0
let marks = 0
let tables = 0
let images = 0
let equations = 0

const walk = (x: any) => {
  if (!x || typeof x !== 'object') return
  if (Array.isArray(x)) return x.forEach(walk)
  if (x.type === 'text') {
    textNodes++
    marks += x.marks?.length ?? 0
  }
  if (x.type === 'table') tables++
  if (x.type === 'image') images++
  if (x.type === 'inlineEquation') equations++
  if (x.type && x.type !== 'doc' && x.type !== 'text') blocks++
  if (x.content) walk(x.content)
}
c.sections.forEach(s => walk(s.json))

console.log('sections        ', c.sections.length)
console.log('claimed words   ', c.totalWords)
console.log('actual words    ', c.sections.reduce((a, s) => a + countWords(s.json), 0))
console.log('blocks          ', blocks)
console.log('text nodes      ', textNodes)
console.log('marks           ', marks, `(${(marks / textNodes).toFixed(2)}/textnode)`)
console.log('tables          ', tables)
console.log('images          ', images)
console.log('equations       ', equations)
console.log('bytes (JSON)    ', (JSON.stringify(c.sections).length / 1e6).toFixed(2), 'MB')
console.log('per-section KB  ', (JSON.stringify(c.sections[0]).length / 1024).toFixed(1))
